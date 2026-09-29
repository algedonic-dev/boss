//! `boss recovery sheet [--tree <dir>] [--facts | --hash | --html]` —
//! the printed way back in, RENDERED from the tree (design 125d405d,
//! backlog fd6d6c08; David 2026-09-28: "we are going to maintain the
//! fidelity of those instructions or generate a new print job").
//!
//! WHY A RENDERER AND NOT A DOCUMENT. The two prose runbooks that
//! played this part had each drifted from the tree by the time anyone
//! read them under stress — one named Kanidm as the human door's
//! recovery while the gateway boots LocalAuth, the other a tmux session
//! `main` while the pod starts `dev`. Both were retyped copies
//! (CLAUDE.md §9a). The sheet's source, `infra/recovery/re-entry.toml`,
//! holds no host, port, URL, path or name: each FACT line names the
//! file that decides a value and where in it, and this module prints
//! what it reads there. A human act is a signature on rendered bytes,
//! never a transcription.
//!
//! READ THE WAY THE FILE'S CONSUMER READS IT. A file that has a Rust
//! reader is first put through it — `infra/estate/estate.toml` through
//! `boss_jobs::estate_seed`, `access.toml` through the dns observer's
//! `parse_access_declaration` — so a file its real consumer would
//! refuse fails the sheet too, rather than being given a second
//! opinion here. A file whose consumer is a shell or kubectl (a
//! manifest, a unit, a script) is read by a one-group PATTERN on the
//! line that defines the value, and every match must capture the SAME
//! value. The estate's flat keys are read by the shell on every host
//! (`render-sor-env.sh`); a test here holds this reader's answer equal
//! to that script's.
//!
//! THE VERSION is a sha256 over the sorted fact set — every resolved
//! line as `file#key = value`, every prose and not-held line with its
//! road and position, every derived dependency — and never over the
//! rendered bytes, which carry a render time and would call every day
//! stale. The tree's sha and the render time are printed beside it and
//! are not in it.
//!
//! WHAT THE PAPER MAY CARRY (design 125d405d Q1): locations, addresses
//! and order, never a value. The renderer refuses a resolved value
//! shaped like key material, every string of the source (words, keys and
//! patterns) shaped like a secret (`secret_in`), and a prose line that
//! carries a literal a fact should have supplied. Those are tripwires
//! for the shapes a regex can see; a short secret or an oblique sentence
//! passes them, and Q3 — which key opens which door — is held by review
//! alone (review of car 67e78997).
//!
//! THE PAPER (car 2): `--pdf FILE` prints the page through the boss-ci
//! image's Chromium, reads the text back out of the PDF and refuses one
//! that does not say every value `--facts` says (`recovery/pdf.rs`),
//! then prints the fact version beside the PDF's sha256 — the version
//! says what the paper states, the digest which bytes were handed over.
//! The footer carries the render time in UTC and in Pacific time, the
//! reader's own.
//!
//! THE REPRINT LOOP (car 3): `boss recovery reprint` is the daily check.
//! The version on paper is the fact hash on the newest presence-signed
//! `print` step of a `reprint-recovery-sheet` packet; when the tree's
//! differs, the check files ONE print job carrying the PDF, or refreshes
//! the open one, and every 90 days it asks that the paper be found where
//! it is kept (`recovery/reprint.rs`).

mod pdf;
mod reprint;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::Result;
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

/// Where the sheet's source lives, relative to the tree.
pub const SOURCE: &str = "infra/recovery/re-entry.toml";

/// The renderer's own shape, in the hash: the fixed words it adds to
/// every sheet ("NOT HELD BY THE TREE", "THIS ROAD NEEDS", …) are code,
/// so a change to them bumps this, and the paper is reprinted.
pub const FORMAT: &str = "recovery-sheet/1";

/// How many characters of the fact hash the page prints as its version
/// — the name a reader compares with the paper in hand, so the footer,
/// `--facts` and the print job's statement all spell it from here.
pub const VERSION_CHARS: usize = 12;

const NOT_EXERCISED: &str = "NOT EXERCISED ON RECORD";
const NOT_HELD: &str = "NOT HELD BY THE TREE";

const ESTATE: &str = "infra/estate/estate.toml";
const ACCESS: &str = "infra/cluster/dns/access.toml";
const INSTANCES: &str = "infra/cluster/instances.toml";
const TUNNEL_ORIGINS: &str = "infra/cluster/tunnel-origins.toml";
const CONNECTOR: &str = "infra/cluster/manifests/cloudflared.yaml";
const GATEWAY_MANIFEST: &str = "infra/cluster/manifests/boss.yaml";

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// The printed way back in when things are broken (design 125d405d).
    #[command(subcommand)]
    Recovery(RecoveryAction),
}

#[derive(clap::Subcommand)]
pub enum RecoveryAction {
    /// Render the recovery sheet from infra/recovery/re-entry.toml: every
    /// fact read from the file that decides it. Default: --facts.
    Sheet {
        /// The tree to render from (a checkout of origin/main).
        #[arg(long, default_value = ".")]
        tree: PathBuf,
        /// Every road and line, each fact with the file#key it came from.
        #[arg(long, conflicts_with_all = ["hash", "html"])]
        facts: bool,
        /// The version: sha256 over the sorted fact set.
        #[arg(long, conflicts_with_all = ["facts", "html"])]
        hash: bool,
        /// The printable page, as HTML on stdout.
        #[arg(long, conflicts_with_all = ["facts", "hash", "pdf"])]
        html: bool,
        /// The printable page as a PDF, written to FILE (`-` for stdout),
        /// printed by the boss-ci image's Chromium and read back before it
        /// is handed over. Prints the fact version and the PDF's sha256.
        #[arg(long, value_name = "FILE", conflicts_with_all = ["facts", "hash", "html"])]
        pdf: Option<PathBuf>,
        /// The Chromium to print with (default: the newest Playwright
        /// headless shell under $PLAYWRIGHT_BROWSERS_PATH,
        /// /opt/ms-playwright or ~/.cache/ms-playwright).
        #[arg(long, value_name = "PATH", requires = "pdf")]
        chromium: Option<PathBuf>,
    },
    /// The daily check (design 125d405d §6): compare the tree's version
    /// with the one signed onto paper, and file or refresh ONE
    /// reprint-recovery-sheet print job carrying the PDF when they
    /// differ — or, when they agree, file the 90-day
    /// confirm-recovery-sheet-kept check once it is due. Writes as
    /// BOSS_ACTOR to BOSS_JOBS_URL and that host's file store; prints one
    /// `verdict: ` line with both versions.
    Reprint {
        /// The tree to render from (a checkout of origin/main).
        #[arg(long, default_value = ".")]
        tree: PathBuf,
        /// The Chromium to print with, as for `sheet --pdf`.
        #[arg(long, value_name = "PATH")]
        chromium: Option<PathBuf>,
    },
}

/// The sheet the tree renders, or every reason it does not.
fn load_or_say(tree: &Path) -> Result<Rendered> {
    load(tree).map_err(|problems| {
        anyhow::anyhow!(
            "the recovery sheet did not render from {} — {} problem(s):\n  {}",
            tree.join(SOURCE).display(),
            problems.len(),
            problems.join("\n  ")
        )
    })
}

/// `now` is the operator's wallclock, read in main.rs: the render time is
/// printed beside the version and is never part of it.
pub async fn dispatch(cmd: Cmd, now: chrono::DateTime<chrono::Utc>) -> Result<()> {
    match cmd {
        Cmd::Recovery(RecoveryAction::Reprint { tree, chromium }) => {
            let sheet = load_or_say(&tree)?;
            let origin_sha = tree_sha(&tree);
            let at = rendered_at(now);
            let rendered = reprint::Tree {
                fact_hash: sheet.hash(),
                facts: sheet.fact_set(),
                origin_sha: origin_sha.clone(),
                rendered_at: at.clone(),
            };
            let wire = crate::steps::Wire::live()?;
            let content = crate::tenant_publish::service_on_door(
                &crate::gate::resolve_jobs_base(None)?,
                "content",
            )?;
            let print = || {
                sheet
                    .print_pdf(&origin_sha, &at, chromium.as_deref())
                    .map(|p| reprint::Pdf {
                        bytes: p.bytes,
                        sha256: p.sha256,
                    })
                    .map_err(|e| anyhow::anyhow!("the recovery sheet did not print: {e}"))
            };
            for line in reprint::check(&wire, &content, &rendered, print, now).await? {
                println!("{line}");
            }
            Ok(())
        }
        Cmd::Recovery(RecoveryAction::Sheet {
            tree,
            facts: _,
            hash,
            html,
            pdf,
            chromium,
        }) => {
            let sheet = load_or_say(&tree)?;
            if let Some(file) = pdf {
                let at = rendered_at(now);
                let printed = sheet
                    .print_pdf(&tree_sha(&tree), &at, chromium.as_deref())
                    .map_err(|e| anyhow::anyhow!("the recovery sheet did not print: {e}"))?;
                // The fact version and the printed bytes' digest, side by
                // side: the version says WHAT the paper states (a reprint
                // is due when it moves), the digest says WHICH bytes were
                // handed over — a PDF carries its print time, so two
                // prints of one version differ, and a signature on paper
                // names the file it signed (design 125d405d §5).
                let stamp = format!(
                    "version  {}\npdf      {}  {}\nrendered {at}\n",
                    sheet.hash(),
                    printed.sha256,
                    file.display()
                );
                if file.as_os_str() == "-" {
                    use std::io::Write;
                    std::io::stdout().write_all(&printed.bytes)?;
                    eprint!("{stamp}");
                } else {
                    std::fs::write(&file, &printed.bytes)
                        .map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
                    print!("{stamp}");
                }
            } else if hash {
                println!("{}", sheet.hash());
            } else if html {
                print!("{}", sheet.html(&tree_sha(&tree), &rendered_at(now)));
            } else {
                print!("{}", sheet.facts_text());
            }
            Ok(())
        }
    }
}

/// The render time as the footer prints it: UTC, which the stack and
/// every packet speak, and Pacific, which the one human holding the
/// paper does (David's Location; CLAUDE.md §Engineering Session Startup,
/// REPORTING TO).
pub fn rendered_at(now: chrono::DateTime<chrono::Utc>) -> String {
    let utc = now.format("%Y-%m-%d %H:%M UTC");
    match pacific_time(now) {
        Some(pt) => format!("{utc} ({pt}, America/Los_Angeles)"),
        None => format!("{utc} (Pacific time not computed)"),
    }
}

/// `now` in America/Los_Angeles, `YYYY-MM-DD HH:MM PDT|PST`.
///
/// THE US RULE, NOT THE TZ DATABASE. Daylight time runs from the second
/// Sunday of March at 02:00 PST (10:00 UTC) to the first Sunday of
/// November at 02:00 PDT (09:00 UTC) — the rule since 2007. One footer
/// timestamp does not earn a timezone-database dependency, and the
/// system's own tzdata cannot be trusted either: a slim image without it
/// answers a missing zone with UTC under the zone's name, a confident
/// wrong time (the "wrong target answers" rule). If Congress moves the
/// clocks, this function and its test move with it. None only for a
/// date chrono cannot build, which no real clock produces.
pub fn pacific_time(now: chrono::DateTime<chrono::Utc>) -> Option<String> {
    use chrono::{Datelike, Duration, NaiveDate, TimeZone, Weekday};
    let year = now.year();
    let nth_sunday = |month: u32, n: u32| {
        NaiveDate::from_weekday_of_month_opt(year, month, Weekday::Sun, n as u8)
    };
    let starts = chrono::Utc.from_utc_datetime(&nth_sunday(3, 2)?.and_hms_opt(10, 0, 0)?);
    let ends = chrono::Utc.from_utc_datetime(&nth_sunday(11, 1)?.and_hms_opt(9, 0, 0)?);
    let (hours, zone) = if now >= starts && now < ends {
        (-7, "PDT")
    } else {
        (-8, "PST")
    };
    Some(format!(
        "{} {zone}",
        (now + Duration::hours(hours)).format("%Y-%m-%d %H:%M")
    ))
}

/// The commit the tree is at, for the page's header — beside the
/// version, never in it.
fn tree_sha(tree: &Path) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(tree)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "not a git checkout".to_string())
}

// ----- the source -------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sheet {
    pub road: Vec<Road>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Road {
    pub id: String,
    pub title: String,
    /// The packet that last exercised this road, if any.
    #[serde(default)]
    pub proven_by: Option<String>,
    #[serde(default)]
    pub needs: Option<Needs>,
    #[serde(default)]
    pub line: Vec<Line>,
}

/// What a road's dependencies are derived from: the host (and path) it
/// reaches through the edge, and/or the manifest of the workload it
/// lands in (a Deployment in the cluster, so the cluster must be up).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Needs {
    #[serde(default)]
    pub host: Option<Ref>,
    #[serde(default)]
    pub path: Option<Ref>,
    #[serde(default)]
    pub workload: Option<String>,
}

/// Where a value lives: a file, and a key or a pattern in it (neither:
/// the file itself).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ref {
    pub file: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub pattern: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Line {
    #[serde(default)]
    pub prose: Option<String>,
    #[serde(default)]
    pub not_tree_held: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub fields: Option<Vec<String>>,
    /// A COMMAND line: a template whose every `{name}` is filled from
    /// the fact `with.name` names — so a command a reader types under
    /// stress is whole on the paper, and every value in it was read.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub with: Option<BTreeMap<String, Ref>>,
}

/// The `{name}` placeholders a command template carries, in order.
fn placeholders(template: &str) -> Vec<String> {
    PLACEHOLDER
        .as_ref()
        .map(|re| {
            re.captures_iter(template)
                .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// A command template's `{name}`. Uncompiled, no template has any
/// placeholder, and parse_sheet refuses every command line.
static PLACEHOLDER: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\{([a-z_]+)\}").ok());

/// Parse the source and judge every line's SHAPE — the reads come
/// later. Every problem is named, not only the first.
pub fn parse_sheet(text: &str) -> Result<Sheet, Vec<String>> {
    let sheet: Sheet = toml::from_str(text).map_err(|e| vec![quiet(SOURCE, &e.to_string())])?;
    let mut problems = Vec::new();
    if sheet.road.is_empty() {
        problems.push(format!(
            "{SOURCE} declares no [[road]] — a sheet with no way in is a file that lost its roads"
        ));
    }
    let mut ids = BTreeSet::new();
    for road in &sheet.road {
        let at = format!("road {:?}", road.id);
        if road.id.is_empty()
            || !road
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            problems.push(format!("{at}: an id is kebab-case"));
        }
        if !ids.insert(road.id.clone()) {
            problems.push(format!("{at} is declared twice"));
        }
        if let Some(lit) = literal_in(&road.title) {
            problems.push(format!(
                "{at} title carries {lit:?} — a value belongs in a fact line"
            ));
        }
        if let Some(kind) = secret_in(&road.title) {
            problems.push(format!("{at} title carries {kind} — {NEVER_ON_PAPER}"));
        }
        let refs = road
            .needs
            .iter()
            .flat_map(|n| n.host.iter().chain(n.path.iter()));
        for r in refs {
            for (what, text) in [("key", &r.key), ("pattern", &r.pattern)] {
                if what == "key" && text.as_deref().is_some_and(secret_key) {
                    problems.push(format!(
                        "{at} needs reads a key whose name says it holds a secret — {NEVER_ON_PAPER}"
                    ));
                }
                if let Some(name) = text
                    .as_deref()
                    .filter(|_| what == "pattern")
                    .and_then(pattern_names_a_secret)
                {
                    problems.push(format!(
                        "{at} needs reads by a pattern whose name says it holds a secret ({name}) — {NEVER_ON_PAPER}"
                    ));
                }
                if let Some(kind) = text.as_deref().and_then(secret_in) {
                    problems.push(format!(
                        "{at} needs {what} carries {kind} — {NEVER_ON_PAPER}"
                    ));
                }
            }
        }
        if let Some(p) = &road.proven_by
            && (p.len() < 8 || !p.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        {
            problems.push(format!("{at}: proven_by {p:?} is not a packet id"));
        }
        if road.line.is_empty() {
            problems.push(format!("{at} has no lines"));
        }
        for (i, line) in road.line.iter().enumerate() {
            let at = format!("{at} line {}", i + 1);
            let fact = line.file.is_some();
            let command = line.command.is_some();
            let kinds = [
                line.prose.is_some(),
                line.not_tree_held.is_some(),
                fact,
                command,
            ];
            if kinds.iter().filter(|k| **k).count() != 1 {
                problems.push(format!(
                    "{at} must be exactly one of prose, not_tree_held, a fact (file) or a command"
                ));
                continue;
            }
            // A command template's placeholders are where facts go; the
            // words around them are held to the prose rule.
            let template = line
                .command
                .as_ref()
                .zip(PLACEHOLDER.as_ref())
                .map(|(c, re)| re.replace_all(c, "X").into_owned());
            for text in [&line.prose, &line.not_tree_held, &line.label, &template]
                .into_iter()
                .flatten()
            {
                if let Some(lit) = literal_in(text) {
                    problems.push(format!(
                        "{at} carries {lit:?} in its words — a host, port, URL, path or name is a FACT line naming the file that decides it, never retyped"
                    ));
                }
            }
            // Every string the source carries — the words, and the key
            // or pattern a fact reads by — is judged for the shapes of a
            // secret. The value it READS is judged again at resolve.
            let refs: Vec<&Ref> = line.with.iter().flat_map(|w| w.values()).collect();
            let mut strings: Vec<(&str, &str)> = [
                ("prose", &line.prose),
                ("not_tree_held", &line.not_tree_held),
                ("label", &line.label),
                ("command", &line.command),
                ("key", &line.key),
                ("pattern", &line.pattern),
            ]
            .into_iter()
            .filter_map(|(k, v)| v.as_deref().map(|s| (k, s)))
            .collect();
            for r in &refs {
                strings.extend(r.key.as_deref().map(|s| ("key", s)));
                strings.extend(r.pattern.as_deref().map(|s| ("pattern", s)));
            }
            for (what, text) in strings {
                if let Some(kind) = secret_in(text) {
                    problems.push(format!("{at} {what} carries {kind} — {NEVER_ON_PAPER}"));
                }
                if what == "key" && secret_key(text) {
                    problems.push(format!(
                        "{at} reads a key whose name says it holds a secret — {NEVER_ON_PAPER}"
                    ));
                }
                if let Some(name) = Some(text)
                    .filter(|_| what == "pattern")
                    .and_then(pattern_names_a_secret)
                {
                    problems.push(format!(
                        "{at} reads by a pattern whose name says it holds a secret ({name}) — {NEVER_ON_PAPER}"
                    ));
                }
            }
            if command {
                if line.label.as_deref().unwrap_or_default().is_empty() {
                    problems.push(format!("{at}: a command line needs a label"));
                }
                let named: BTreeSet<String> = line
                    .with
                    .as_ref()
                    .map(|w| w.keys().cloned().collect())
                    .unwrap_or_default();
                let used: BTreeSet<String> = line
                    .command
                    .as_deref()
                    .map(placeholders)
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                if named != used || used.is_empty() {
                    problems.push(format!(
                        "{at}: a command's placeholders {used:?} must be exactly the facts its `with` names {named:?}"
                    ));
                }
                if line.key.is_some() || line.pattern.is_some() || line.fields.is_some() {
                    problems.push(format!("{at}: a command reads its facts through `with`"));
                }
            } else if line.with.is_some() {
                problems.push(format!("{at}: `with` belongs to a command line"));
            }
            if command {
                continue;
            }
            if fact {
                if line.label.as_deref().unwrap_or_default().is_empty() {
                    problems.push(format!("{at}: a fact line needs a label"));
                }
                if line.key.is_some() && line.pattern.is_some() {
                    problems.push(format!("{at}: a fact reads by key OR by pattern, not both"));
                }
            } else if line.label.is_some()
                || line.key.is_some()
                || line.pattern.is_some()
                || line.fields.is_some()
            {
                problems.push(format!(
                    "{at}: label/key/pattern/fields belong to a fact line"
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(sheet)
    } else {
        Err(problems)
    }
}

/// The first literal a prose line must not carry: an address, a URL, a
/// dotted name (a hostname, a file, an e-mail), an absolute or home
/// path, or a port. Deliberately wide: the paper's words are procedure,
/// and anything a reader could type into a terminal comes from a fact.
pub fn literal_in(text: &str) -> Option<String> {
    static SHAPES: LazyLock<Option<Vec<Regex>>> = LazyLock::new(|| {
        compiled(&[
            r"\b\d{1,3}(?:\.\d{1,3}){3}\b",
            r"[A-Za-z][A-Za-z0-9+.-]*://\S*",
            r"[A-Za-z0-9_-]+(?:@[A-Za-z0-9_-]+)?\.[A-Za-z][A-Za-z0-9-]+",
            r"(?:^|[\s(`'\x22])~?/[A-Za-z0-9_.-]+",
            r":\d{2,5}\b",
        ])
    });
    match SHAPES.as_ref() {
        Some(shapes) => shapes
            .iter()
            .find_map(|re| re.find(text).map(|m| m.as_str().trim().to_string())),
        None => Some(UNCOMPILED.to_string()),
    }
}

/// What a guard answers when its own shapes did not compile: it fails
/// CLOSED, refusing everything, rather than letting everything through.
const UNCOMPILED: &str = "a guard whose shapes did not compile";

/// Compile a guard's shapes once, all or none.
fn compiled(shapes: &[&str]) -> Option<Vec<Regex>> {
    shapes.iter().map(|s| Regex::new(s).ok()).collect()
}

const NEVER_ON_PAPER: &str =
    "the paper carries locations, addresses and order, never a value (design 125d405d Q1)";

/// Which shape of secret a string the SOURCE carries has, if any — never
/// the secret itself, so a refusal cannot print what it refused (review
/// of car 67e78997, F1/F7). Run over every string in the source: title,
/// label, prose, not_tree_held, command, key and pattern.
///
/// WHAT THIS ENFORCES, AND WHAT IT CANNOT. It is a TRIPWIRE for the
/// obvious spellings: key material ([`looks_like_key_material`]), a
/// secret stated ("the PIN is …", "password: …"), a bare number of four
/// or more digits (a PIN or a code; a port or a count is a fact, so
/// prose has no business with one), and a sentence that ties a
/// particular key or device to a door ("the YubiKey on the blue keychain
/// opens the LAN door"). A determined or oblique phrasing passes it.
/// Design Q3 — which key opens which door, and which device holds it —
/// is enforced by REVIEW, and the source's header says so.
pub fn secret_in(text: &str) -> Option<&'static str> {
    secret_shape(text, true)
}

/// The same judgement for a VALUE a fact read or a command filled
/// (round-2 review R2-3): every shape but the bare number, because a
/// value is where a real port or count lives (51820, 19531), and prose
/// is where one never belongs.
pub fn secret_in_value(value: &str) -> Option<&'static str> {
    secret_shape(value, false)
}

/// The words that name a secret, read as the LAST SEGMENT of a key
/// (`pin`, `forge_token`, `admin-password`): a fact read at such a key is
/// refused before its value is looked at (R2-3).
pub fn secret_key(key: &str) -> bool {
    let last = segments(key)
        .ok()
        .and_then(|s| s.last().map(|(name, _)| name.clone()))
        .unwrap_or_else(|| key.to_string());
    secret_name(&last)
}

/// The words of a name, lower-cased: split at `_`, `-` and every other
/// non-alphanumeric, and at camelCase boundaries (`forgeTokenFile` →
/// forge, token, file; `APIKey` → api, key). One split, so snake, kebab
/// and camel spellings of a name are judged by one rule (review of car
/// 2, F4: camelCase escaped the "anywhere" words).
fn name_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            words.push(std::mem::take(&mut cur));
            continue;
        }
        let prev = i.checked_sub(1).and_then(|j| chars.get(j)).copied();
        let next = chars.get(i + 1).copied();
        // An acronym's plural (`PINs`, `OTPs`, `JWTs`): its lowercase
        // letter is a lone `s` that ends the word — at the name's end, a
        // separator or the next capital — and belongs to the acronym,
        // not to a new word starting at its last capital (review of
        // car 2, R2-A: `PINs` read as `pi`, `ns` and was not refused).
        let plural_s = next == Some('s') && !chars.get(i + 2).is_some_and(|c| c.is_lowercase());
        let camel = c.is_uppercase()
            && prev.is_some_and(|p| {
                p.is_lowercase()
                    || p.is_ascii_digit()
                    || (p.is_uppercase() && next.is_some_and(char::is_lowercase) && !plural_s)
            });
        if camel {
            words.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_lowercase());
    }
    words.push(cur);
    words.retain(|w| !w.is_empty());
    words
}

/// Whether one NAME — a key's last segment, or an identifier a pattern
/// matches before its capture — says the value it names is a secret.
/// Judged over [`name_words`], so every spelling meets one rule:
///
///   - a word that only ever names a secret — `password`, `token`,
///     `secret`, `pwd`, `creds`, `bearer`, `cookie`, `jwt`,
///     `authorization`, … — or a pair that does (`api key`, `private
///     key`, `recovery code`) counts ANYWHERE: `forge_token_file` and
///     `forgeTokenFile` are both refused (R2-3, F4);
///   - four words — `code`, `pass`, `key`, `auth` — are also ordinary
///     words in a LOCATION's name (`auth_provider`, `key_path`,
///     `ssh_key_file`, `pass_file`, the gateway's `BOSS_AUTH_PROVIDER`
///     the web road reads), so they name a secret as the LAST word
///     (`api_key`, `basic_auth`, `db_pass`), or just before a last word
///     that carries a value (`authHeader`, `pass_phrase`, `KEY_PEM`,
///     `apiKeyValue`, `key_material`);
///   - a name whose last word is `name` or `id` names an OBJECT, not its
///     contents — `secretName` is the Kubernetes field naming a Secret,
///     which is a location the web road reads by.
///
/// Wide on purpose: `publicKey` and `exit_code` are refused too, which
/// costs a rename and never a leak. A tripwire, not a proof — a name
/// chosen to hide passes, and review holds the rest.
fn secret_name(name: &str) -> bool {
    const ALWAYS: [&str; 18] = [
        "pin",
        "password",
        "passphrase",
        "passcode",
        "passwd",
        "pwd",
        "pw",
        "secret",
        "token",
        "otp",
        "totp",
        "seed",
        "credential",
        "creds",
        "bearer",
        "cookie",
        "jwt",
        "authorization",
    ];
    const PAIRS: [(&str, &str); 3] = [("api", "key"), ("private", "key"), ("recovery", "code")];
    const ENDS: [&str; 4] = ["code", "pass", "key", "auth"];
    const VALUE_ENDS: [&str; 8] = [
        "value", "header", "phrase", "data", "pem", "material", "hash", "string",
    ];
    // Singular: `tokens`, `keys`, `codes` — but `pass` is its own word.
    let is = |w: &str, set: &[&str]| {
        set.contains(&w) || w.strip_suffix('s').is_some_and(|s| set.contains(&s))
    };
    let words = name_words(name);
    let Some(last) = words.last() else {
        return false;
    };
    if last == "name" || last == "id" {
        return false;
    }
    let always = words.iter().any(|w| is(w, &ALWAYS));
    let pair = words
        .windows(2)
        .any(|p| PAIRS.iter().any(|(a, b)| p[0] == *a && is(&p[1], &[b])));
    let ends = is(last, &ENDS);
    let value_after =
        words.len() >= 2 && is(last, &VALUE_ENDS) && is(&words[words.len() - 2], &ENDS);
    always || pair || ends || value_after
}

/// The first identifier a PATTERN matches before its capture that names
/// a secret (round-3 R3-2): `^Environment=BOSS_ADMIN_PASSWORD=(\S+)$`
/// and `\{name: DB_PASS, value: "([^"]+)"\}` read a value by its name as
/// surely as a key does. Every identifier in the literal is judged, not
/// only the nearest — in a manifest's env list the nearest is `value`,
/// and the name is the one before it. Escapes (`\s`, `\{`) are dropped
/// first so their letters are not read as words. Returned so a refusal
/// can say which word; it is a name the SOURCE carries, never a value.
fn pattern_names_a_secret(pattern: &str) -> Option<String> {
    static ESCAPE: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\\.").ok());
    static IDENT: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"[A-Za-z][A-Za-z0-9_-]*").ok());
    let (Some(escape), Some(ident)) = (ESCAPE.as_ref(), IDENT.as_ref()) else {
        return Some(UNCOMPILED.to_string());
    };
    let plain = escape.replace_all(pattern, " ");
    // The literal ends at the first group that captures: `(` not
    // followed by `?`.
    let left = plain
        .char_indices()
        .find(|&(i, c)| c == '(' && !plain[i + 1..].starts_with('?'))
        .map(|(i, _)| &plain[..i])
        .unwrap_or(&plain);
    ident
        .find_iter(left)
        .map(|m| m.as_str().trim_end_matches(['_', '-']))
        .find(|name| secret_name(name))
        .map(String::from)
}

fn secret_shape(text: &str, numbers: bool) -> Option<&'static str> {
    const KINDS: [&str; 3] = [
        "a secret stated",
        "a bare number of four or more digits (a PIN or a code)",
        "a key or device tied to a door (design Q3)",
    ];
    static SHAPES: LazyLock<Option<Vec<Regex>>> = LazyLock::new(|| {
        compiled(&[
            r"(?i)\b(?:pin|password|passphrase|passcode|passwd|recovery codes?|secret|token|otp|totp seed)\b\s*(?:is|was|=|:)\s*\S",
            r"\b\d{4,}\b",
            r"(?i)\b(?:the|my|our|your|this|that)\s+(?:[\w-]+\s+){0,6}?(?:key|yubikey|token|card|stick|laptop|phone|device|keyfile)\b[^.;]{0,80}?\b(?:opens|unlocks|is the key to|is for)\b",
        ])
    });
    if looks_like_key_material(text) {
        return Some("key material");
    }
    match SHAPES.as_ref() {
        Some(shapes) => shapes
            .iter()
            .zip(KINDS)
            .enumerate()
            .filter(|(i, _)| numbers || *i != 1)
            .find_map(|(_, (re, kind))| re.is_match(text).then_some(kind)),
        None => Some(UNCOMPILED),
    }
}

/// An error from a parser, without the source line it quotes: a TOML
/// error prints the offending line, and a gate log or a transcript is
/// not a place for whatever that line held (review F7).
fn quiet(rel: &str, err: &str) -> String {
    let kept: Vec<&str> = err
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with('|')
                || t.split_once(" |")
                    .is_some_and(|(n, _)| n.chars().all(|c| c.is_ascii_digit())))
        })
        .filter(|l| !l.trim().is_empty())
        .collect();
    format!("{rel}: {}", kept.join(" "))
}

// ----- the reads --------------------------------------------------------

/// A resolved value: text, or a list of tables printed field by field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Items(Vec<Vec<(String, String)>>),
}

impl Value {
    fn canonical(&self) -> String {
        match self {
            Value::Text(t) => t.clone(),
            Value::Items(items) => items
                .iter()
                .map(|item| {
                    item.iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .collect::<Vec<_>>()
                .join(" || "),
        }
    }
    fn texts(&self) -> Vec<&str> {
        match self {
            Value::Text(t) => vec![t.as_str()],
            Value::Items(items) => items
                .iter()
                .flat_map(|i| i.iter().map(|(_, v)| v.as_str()))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineOut {
    Fact {
        label: String,
        source: String,
        value: Value,
    },
    Prose(String),
    NotHeld(String),
}

#[derive(Debug, Clone)]
pub struct RoadOut {
    pub id: String,
    pub title: String,
    pub proven_by: Option<String>,
    pub lines: Vec<LineOut>,
    /// Derived: what this road needs up, and what it does not open.
    pub needs: Vec<String>,
    /// Every file this road's lines and dependencies were read from.
    pub files: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct Rendered {
    pub roads: Vec<RoadOut>,
}

/// A printed sheet, already read back: the PDF's bytes and their sha256.
pub struct Printed {
    pub bytes: Vec<u8>,
    pub sha256: String,
}

/// The files the renderer reads, each once, each through its
/// consumer's check.
struct Tree<'a> {
    root: &'a Path,
    texts: BTreeMap<String, Result<String, String>>,
}

impl<'a> Tree<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            texts: BTreeMap::new(),
        }
    }

    fn text(&mut self, rel: &str) -> Result<String, String> {
        let root = self.root;
        self.texts
            .entry(rel.to_string())
            .or_insert_with(|| {
                std::fs::read_to_string(root.join(rel))
                    .map_err(|e| format!("{rel}: {e}"))
                    .and_then(|t| consumer_check(rel, &t).map(|()| t))
            })
            .clone()
    }

    fn resolve(&mut self, r: &Ref, fields: Option<&[String]>) -> Result<(String, Value), String> {
        let (source, value) = self.read(r, fields)?;
        // The value is judged the way prose is, less the bare-number rule
        // (a value is where a port lives) — round-2 review R2-3. Named by
        // kind and length, never printed.
        if let Some((kind, len)) = value
            .texts()
            .into_iter()
            .find_map(|t| secret_in_value(t).map(|k| (k, t.len())))
        {
            return Err(format!(
                "{source} resolves to {kind} ({len} chars) — {NEVER_ON_PAPER}"
            ));
        }
        Ok((source, value))
    }

    fn read(&mut self, r: &Ref, fields: Option<&[String]>) -> Result<(String, Value), String> {
        // A file that declares a Kubernetes Secret is never READ for a
        // value, whatever the key or pattern (R2-3): what it holds is a
        // credential by kind. Naming it as a file (neither key nor
        // pattern) prints only its path, which is a location.
        if r.key.is_some() || r.pattern.is_some() {
            let text = self.text(&r.file)?;
            if declares_a_secret(&text) {
                return Err(format!(
                    "{}: declares `kind: Secret`, so no fact is read from it — {NEVER_ON_PAPER}",
                    r.file
                ));
            }
        }
        match (&r.key, &r.pattern) {
            (Some(key), None) => {
                let source = format!("{}#{key}", r.file);
                let text = self.text(&r.file)?;
                let doc = data_file(&r.file, &text)?;
                let found = walk(&doc, key).map_err(|e| format!("{source}: {e}"))?;
                let value = printable(&r.file, &doc, &found, fields)
                    .map_err(|e| format!("{source}: {e}"))?;
                Ok((source, value))
            }
            (None, Some(pattern)) => {
                let source = format!("{}#/{pattern}/", r.file);
                let text = self.text(&r.file)?;
                let value = capture(&text, pattern).map_err(|e| format!("{source}: {e}"))?;
                Ok((source, Value::Text(value)))
            }
            (None, None) => {
                if self.root.join(&r.file).is_file() {
                    Ok((r.file.clone(), Value::Text(r.file.clone())))
                } else {
                    Err(format!(
                        "{}: the file this line names does not exist",
                        r.file
                    ))
                }
            }
            (Some(_), Some(_)) => Err(format!("{}: a key OR a pattern, not both", r.file)),
        }
    }

    /// Fill a command template from its facts. The source names every
    /// fact it was filled from, in placeholder order.
    fn command(
        &mut self,
        template: &str,
        with: &BTreeMap<String, Ref>,
    ) -> Result<(String, Value, Vec<String>), String> {
        let mut values = BTreeMap::new();
        let mut sources = Vec::new();
        let mut files = Vec::new();
        for name in placeholders(template) {
            let r = with
                .get(&name)
                .ok_or_else(|| format!("no fact named {name:?} in `with`"))?;
            let (source, value) = self.resolve(r, None)?;
            let Value::Text(v) = value else {
                return Err(format!("{source}: a command fills from single values"));
            };
            values.insert(name.clone(), v);
            sources.push(format!("{name}={source}"));
            files.push(r.file.clone());
        }
        let filled = fill_once(template, &values)
            .ok_or_else(|| "the placeholder pattern did not compile".to_string())?;
        Ok((sources.join("; "), Value::Text(filled), files))
    }
}

/// Fill every `{name}` in ONE pass over the template: a filled value is
/// never scanned again, so a value that itself contains `{other}` prints
/// as written rather than being filled a second time (round-2 review
/// R2-6). A name with no value is left as written. None only when the
/// placeholder pattern did not compile.
fn fill_once(template: &str, values: &BTreeMap<String, String>) -> Option<String> {
    PLACEHOLDER.as_ref().map(|re| {
        re.replace_all(template, |c: &regex::Captures| {
            let whole = c.get(0).map(|m| m.as_str()).unwrap_or_default();
            c.get(1)
                .and_then(|n| values.get(n.as_str()))
                .cloned()
                .unwrap_or_else(|| whole.to_string())
        })
        .into_owned()
    })
}

/// Whether a manifest declares a Kubernetes Secret anywhere in it, in
/// any spelling kubectl accepts (round-3 R3-2): a block line, a list
/// item (`- kind: Secret`), the value quoted either way, a trailing
/// comment, flow style (`{kind: Secret, …}`) and JSON
/// (`"kind":"Secret"`). The kind must END at a quote, a separator or
/// the line, so `SecretProviderClass` is not one. Uncompiled, every file
/// declares one — the read is refused, never let through.
fn declares_a_secret(text: &str) -> bool {
    static KIND: LazyLock<Option<Regex>> = LazyLock::new(|| {
        Regex::new(r#"(?m)(?:^|[{,\s-])["']?kind["']?\s*:\s*["']?Secret["']?\s*(?:[,}]|#.*$|$)"#)
            .ok()
    });
    KIND.as_ref().is_none_or(|re| re.is_match(text))
}

/// A file with a Rust consumer is judged by it first, so the sheet never
/// prints a value its real reader would refuse.
fn consumer_check(rel: &str, text: &str) -> Result<(), String> {
    match rel {
        ESTATE => boss_jobs::estate_seed::parse_estate_toml(text)
            .map(|_| ())
            .map_err(|e| quiet(rel, &format!("boss_jobs::estate_seed refuses it: {e}"))),
        ACCESS => access_declaration(text)
            .map(|_| ())
            .map_err(|e| quiet(rel, &format!("the dns observer refuses it: {e}"))),
        _ => Ok(()),
    }
}

/// access.toml through the dns observer's own parse, for the zone the
/// file says it is about.
fn access_declaration(
    text: &str,
) -> Result<boss_dispatcher_handlers::handlers::dns_observe::AccessDeclaration, String> {
    let zone = toml::from_str::<toml::Table>(text)
        .ok()
        .and_then(|t| {
            t.get("account_zone")
                .and_then(|z| z.as_str().map(String::from))
        })
        .unwrap_or_default();
    boss_dispatcher_handlers::handlers::dns_observe::parse_access_declaration(text, &zone)
}

fn data_file(rel: &str, text: &str) -> Result<Json, String> {
    if rel.ends_with(".toml") {
        let table: toml::Table = toml::from_str(text).map_err(|e| quiet(rel, &e.to_string()))?;
        serde_json::to_value(table).map_err(|e| quiet(rel, &e.to_string()))
    } else if rel.ends_with(".json") {
        serde_json::from_str(text).map_err(|e| quiet(rel, &e.to_string()))
    } else {
        Err(format!(
            "{rel}: a key reads a .toml or .json file; read anything else by pattern"
        ))
    }
}

#[derive(Debug)]
enum Sel {
    /// `[field=value]`: the ONE table whose field equals the value.
    Eq(String, String),
    /// `[field~value]`: every table whose field is, or holds, the value.
    Has(String, String),
}

fn segments(path: &str) -> Result<Vec<(String, Option<Sel>)>, String> {
    let mut raw = Vec::new();
    let (mut cur, mut depth) = (String::new(), 0usize);
    for c in path.chars() {
        match c {
            '[' => {
                depth += 1;
                cur.push(c);
            }
            ']' => {
                depth = depth.checked_sub(1).ok_or("an unmatched ]")?;
                cur.push(c);
            }
            '.' if depth == 0 => raw.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    raw.push(cur);
    raw.into_iter()
        .map(|seg| {
            let Some((name, rest)) = seg.split_once('[') else {
                return if seg.is_empty() {
                    Err("an empty segment".to_string())
                } else {
                    Ok((seg, None))
                };
            };
            let inner = rest
                .strip_suffix(']')
                .ok_or_else(|| format!("segment {seg:?} is not name[field=value]"))?;
            let sel = if let Some((f, v)) = inner.split_once('=') {
                Sel::Eq(f.to_string(), v.to_string())
            } else if let Some((f, v)) = inner.split_once('~') {
                Sel::Has(f.to_string(), v.to_string())
            } else {
                return Err(format!(
                    "segment {seg:?}: a selector is [field=value] or [field~value]"
                ));
            };
            Ok((name.to_string(), Some(sel)))
        })
        .collect()
}

fn scalar_text(v: &Json) -> Option<String> {
    match v {
        Json::String(s) => Some(s.clone()),
        Json::Number(n) => Some(n.to_string()),
        Json::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Follow a key path through a document.
fn walk(doc: &Json, path: &str) -> Result<Json, String> {
    let mut cur = doc.clone();
    for (name, sel) in segments(path)? {
        cur = match &cur {
            Json::Object(map) => map
                .get(&name)
                .cloned()
                .ok_or_else(|| format!("no key {name:?}"))?,
            // A name applied to a list is that field of every table in it.
            Json::Array(items) => Json::Array(
                items
                    .iter()
                    .map(|i| {
                        i.get(&name)
                            .cloned()
                            .ok_or_else(|| format!("a table in the list has no {name:?}"))
                    })
                    .collect::<Result<_, _>>()?,
            ),
            _ => return Err(format!("{name:?} is asked of a value that is not a table")),
        };
        let Some(sel) = sel else { continue };
        let Json::Array(items) = &cur else {
            return Err(format!("{name:?} is not a list, so it takes no selector"));
        };
        cur = match sel {
            Sel::Eq(field, want) => {
                let hits: Vec<&Json> = items
                    .iter()
                    .filter(|i| {
                        i.get(&field).and_then(scalar_text).as_deref() == Some(want.as_str())
                    })
                    .collect();
                match hits.as_slice() {
                    [one] => (*one).clone(),
                    [] => return Err(format!("no {name} has {field} = {want:?}")),
                    many => {
                        return Err(format!(
                            "{} {name} tables have {field} = {want:?}; [field=value] must pick exactly one",
                            many.len()
                        ));
                    }
                }
            }
            Sel::Has(field, want) => {
                let hits: Vec<Json> = items
                    .iter()
                    .filter(|i| match i.get(&field) {
                        Some(Json::Array(vs)) => vs
                            .iter()
                            .any(|v| scalar_text(v).as_deref() == Some(want.as_str())),
                        Some(v) => scalar_text(v).as_deref() == Some(want.as_str()),
                        None => false,
                    })
                    .cloned()
                    .collect();
                if hits.is_empty() {
                    return Err(format!("no {name} has {field} holding {want:?}"));
                }
                Json::Array(hits)
            }
        };
    }
    Ok(cur)
}

/// The one data file whose strings are templated on its own top-level
/// string keys — `{host}` in its steps is its `host`, the same fill the
/// /it/estate page applies (estate.ts devDoorSteps). No other file is
/// filled (round-2 review R2-6): a value read from anywhere else prints
/// exactly as written.
const DEV_DOOR: &str = "apps/web/src/it/estate/dev-door.json";

fn fill(rel: &str, doc: &Json, s: &str) -> Result<String, String> {
    let Json::Object(top) = doc else {
        return Ok(s.to_string());
    };
    if rel != DEV_DOOR {
        return Ok(s.to_string());
    }
    let values: BTreeMap<String, String> = top
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect();
    fill_once(s, &values).ok_or_else(|| "the placeholder pattern did not compile".to_string())
}

fn printable(rel: &str, doc: &Json, v: &Json, fields: Option<&[String]>) -> Result<Value, String> {
    if let Some(fields) = fields {
        let Json::Array(items) = v else {
            return Err("`fields` asks for a list of tables".to_string());
        };
        return items
            .iter()
            .map(|item| {
                fields
                    .iter()
                    .map(|f| {
                        let t = item
                            .get(f)
                            .and_then(scalar_text)
                            .ok_or_else(|| format!("an item has no text field {f:?}"))?;
                        Ok((f.clone(), fill(rel, doc, &t)?))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Items);
    }
    let text = match v {
        Json::Array(vs) => {
            let parts = vs
                .iter()
                .map(|x| scalar_text(x).ok_or("a list holding tables needs `fields`"))
                .collect::<Result<Vec<_>, _>>()?;
            if parts.is_empty() {
                return Err("an empty list — nothing to print".to_string());
            }
            parts.join(", ")
        }
        other => scalar_text(other).ok_or("not a printable value (a table)")?,
    };
    Ok(Value::Text(fill(rel, doc, &text)?))
}

/// Every match of a one-group pattern must capture the same value.
fn capture(text: &str, pattern: &str) -> Result<String, String> {
    let re = RegexBuilder::new(pattern)
        .multi_line(true)
        .build()
        .map_err(|e| format!("the pattern does not compile: {e}"))?;
    if re.captures_len() != 2 {
        return Err("a pattern carries exactly one capture group".to_string());
    }
    // Each distinct value with the line it was first found on. A refusal
    // names LINES, never values: what a pattern captured may be exactly
    // what must not reach a gate log (review F7).
    let mut found: BTreeMap<String, usize> = BTreeMap::new();
    for c in re.captures_iter(text) {
        if let Some(m) = c.get(1) {
            let line = text[..m.start()].matches('\n').count() + 1;
            found.entry(m.as_str().to_string()).or_insert(line);
        }
    }
    let mut values = found.iter();
    match (values.next(), values.next()) {
        (Some((one, _)), None) if !one.is_empty() => Ok(one.clone()),
        (None, _) | (Some(_), None) => {
            Err("the pattern matches nothing — the line it read has moved or gone".to_string())
        }
        _ => {
            let mut lines: Vec<usize> = found.values().copied().collect();
            lines.sort_unstable();
            Err(format!(
                "the pattern matches {} different values (first seen on lines {lines:?}) — the file spells this fact more than once",
                found.len()
            ))
        }
    }
}

/// Whether a value is shaped like key material: a PEM armour line, an
/// OpenSSH public or private key blob, a run of 40+ hex digits, or a
/// run of 40+ base64 characters mixing upper case and digits.
///
/// A BACKSTOP, NOT THE GUARD (review of car 67e78997, F6). It misses a
/// short secret — a PIN, a password, a base32 TOTP seed, 32 hex digits,
/// base64url broken by `-` into short runs. The real guard is that a
/// fact reads only a file in the tree, which the no-secrets lint already
/// covers, and that a fact names a location, which review holds. A
/// lower-case path of the same length is not key material, which is why
/// the run alone is not the test.
pub fn looks_like_key_material(v: &str) -> bool {
    static SHAPES: LazyLock<Option<Vec<Regex>>> = LazyLock::new(|| {
        compiled(&[
            r"[A-Za-z0-9+/=_]{40,}",
            r"\b(?:ssh-(?:ed25519|rsa|dss)|ecdsa-sha2-nistp\d+|sk-ssh-ed25519@openssh\.com)\s+AAAA",
        ])
    });
    let Some([run, ssh]) = SHAPES.as_deref() else {
        return true;
    };
    v.contains("-----BEGIN")
        || ssh.is_match(v)
        || run.find_iter(v).any(|m| {
            let s = m.as_str();
            s.chars().all(|c| c.is_ascii_hexdigit())
                || (s.chars().any(|c| c.is_ascii_uppercase())
                    && s.chars().any(|c| c.is_ascii_digit()))
        })
}

// ----- derived dependencies ---------------------------------------------

fn host_of(v: &str) -> String {
    let rest = v.split_once("://").map(|(_, r)| r).unwrap_or(v);
    rest.split(['/', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// What a road through `host` (and `path`) needs up, read from what
/// stands in front of it, what routes it, and what the gateway serves
/// on the LAN. Returns the lines and the files they came from.
fn derive_needs(
    tree: &mut Tree,
    host: &str,
    path: &str,
) -> Result<(Vec<String>, Vec<&'static str>), String> {
    let mut needs = Vec::new();
    let mut files = vec![ACCESS, INSTANCES, TUNNEL_ORIGINS];

    // In front: the most specific Access application covering the path.
    let access = access_declaration(&tree.text(ACCESS)?)?;
    let covering = access
        .application
        .iter()
        .filter(|a| {
            let (h, p) = a
                .domain
                .split_once('/')
                .map(|(h, p)| (h, format!("/{p}")))
                .unwrap_or((a.domain.as_str(), String::new()));
            h == host && (p.is_empty() || path.starts_with(&p))
        })
        .max_by_key(|a| a.domain.len());
    match covering {
        Some(app) if app.policy.iter().any(|p| p.decision == "bypass") => needs.push(format!(
            "Cloudflare Access lets this path through without a sign-in (application {:?}, bypass)",
            app.name
        )),
        Some(app) => {
            let emails: Vec<&str> = app
                .policy
                .iter()
                .filter(|p| p.decision == "allow")
                .flat_map(|p| p.include.emails.iter().map(String::as_str))
                .collect();
            let whom = if emails.is_empty() {
                "an identity the account's dashboard admits".to_string()
            } else {
                emails.join(", ")
            };
            let scope = if app.domain.contains('/') {
                format!("the path {}", app.domain)
            } else {
                format!("the WHOLE host {}", app.domain)
            };
            if app.app_type == "ssh" {
                needs.push(format!(
                    "a Cloudflare Access sign-in as {whom}, which issues the session's certificate (application {:?} on {scope})",
                    app.name
                ));
            } else {
                needs.push(format!(
                    "a Cloudflare Access sign-in first: a one-time code sent to {whom} (application {:?} on {scope}), so that mailbox must be reachable",
                    app.name
                ));
            }
        }
        None => needs.push("no Cloudflare Access application stands in front of it".to_string()),
    }

    // Routing: an instance's hostname or a declared non-instance origin
    // rides the tunnel, whose connector's manifest says where it runs.
    let instances: toml::Table =
        toml::from_str(&tree.text(INSTANCES)?).map_err(|e| quiet(INSTANCES, &e.to_string()))?;
    let is_instance = instances
        .values()
        .filter_map(|v| v.as_table())
        .any(|t| t.get("hostname").and_then(|h| h.as_str()) == Some(host));
    let origins: toml::Table = toml::from_str(&tree.text(TUNNEL_ORIGINS)?)
        .map_err(|e| quiet(TUNNEL_ORIGINS, &e.to_string()))?;
    let is_origin = origins
        .get("origin")
        .and_then(|o| o.as_array())
        .is_some_and(|os| {
            os.iter()
                .any(|o| o.get("hostname").and_then(|h| h.as_str()) == Some(host))
        });
    if is_instance || is_origin {
        files.push(CONNECTOR);
        let connector = tree.text(CONNECTOR)?;
        let in_cluster =
            connector.contains("kind: Deployment") && connector.contains("name: cloudflared");
        needs.push(if in_cluster {
            "Cloudflare's edge up, and the tunnel connector, which runs INSIDE the cluster — so the cluster must be up".to_string()
        } else {
            "Cloudflare's edge up, and the tunnel connector".to_string()
        });
    } else {
        needs.push(
            "no tunnel route: this name is not served through Cloudflare's tunnel".to_string(),
        );
    }

    // The LAN: an instance's gateway Service, and whether it speaks TLS.
    if is_instance {
        files.push(GATEWAY_MANIFEST);
        let manifest = tree.text(GATEWAY_MANIFEST)?;
        // The Service's first port and its name. Not found is an error,
        // never a quieter sheet: a road that stops saying it has no LAN
        // way in would read as though it had one.
        let re = RegexBuilder::new(
            r"^kind: Service\nmetadata:\n  name: boss-gateway\n(?:.*\n)*?\s+- \{port: (\d+), targetPort: \d+, name: ([a-z-]+)\}",
        )
        .multi_line(true)
        .build()
        .map_err(|e| e.to_string())?;
        let (port, name) = re
            .captures(&manifest)
            .and_then(|c| Some((c.get(1)?.as_str().to_string(), c.get(2)?.as_str().to_string())))
            .ok_or_else(|| {
                format!("{GATEWAY_MANIFEST}: no Service boss-gateway port found — cannot say what the LAN offers this host")
            })?;
        needs.push(if name == "http" {
            format!(
                "the gateway up. Its only LAN address serves plain HTTP (Service boss-gateway, port {port}), where a passkey cannot run — so there is no LAN road to this door"
            )
        } else {
            format!("the gateway up (its LAN Service port {port} is named {name})")
        });
    }
    Ok((needs, files))
}

/// What a road that lands in a cluster workload needs: the cluster, and
/// that workload running — read off the manifest's first Deployment, so
/// a workload that moves out of the cluster changes the line by itself.
fn derive_workload_needs(tree: &mut Tree, manifest: &str) -> Result<String, String> {
    let text = tree.text(manifest)?;
    let re = RegexBuilder::new(r"^kind: Deployment\nmetadata:\n  name: (\S+)\n  namespace: (\S+)$")
        .multi_line(true)
        .build()
        .map_err(|e| e.to_string())?;
    let c = re.captures(&text).ok_or_else(|| {
        format!("{manifest}: declares no Deployment — cannot say what this road lands in")
    })?;
    let (name, ns) = (
        c.get(1).map(|m| m.as_str()).unwrap_or_default(),
        c.get(2).map(|m| m.as_str()).unwrap_or_default(),
    );
    Ok(format!(
        "the cluster up, and Deployment {name} running in namespace {ns} — the workspace is a pod, so a dark cluster closes this road"
    ))
}

// ----- the whole sheet ---------------------------------------------------

/// Read, parse and resolve the tree's sheet. Every problem is named.
pub fn load(tree: &Path) -> Result<Rendered, Vec<String>> {
    let text =
        std::fs::read_to_string(tree.join(SOURCE)).map_err(|e| vec![format!("{SOURCE}: {e}")])?;
    let sheet = parse_sheet(&text)?;
    resolve(tree, &sheet)
}

pub fn resolve(root: &Path, sheet: &Sheet) -> Result<Rendered, Vec<String>> {
    let mut tree = Tree::new(root);
    let mut problems = Vec::new();
    let mut roads = Vec::new();
    for road in &sheet.road {
        let mut files = BTreeSet::from([SOURCE.to_string()]);
        let mut lines = Vec::new();
        for (i, line) in road.line.iter().enumerate() {
            let at = format!("road {:?} line {}", road.id, i + 1);
            if let Some(p) = &line.prose {
                lines.push(LineOut::Prose(p.clone()));
            } else if let Some(n) = &line.not_tree_held {
                lines.push(LineOut::NotHeld(n.clone()));
            } else if let (Some(template), Some(with)) = (&line.command, &line.with) {
                match tree.command(template, with) {
                    Ok((source, value, from)) => {
                        files.extend(from);
                        if value.texts().into_iter().any(looks_like_key_material) {
                            problems.push(format!(
                                "{at} ({source}) fills to what looks like key material — {NEVER_ON_PAPER}"
                            ));
                        }
                        lines.push(LineOut::Fact {
                            label: line.label.clone().unwrap_or_default(),
                            source,
                            value,
                        });
                    }
                    Err(e) => problems.push(format!("{at}: {e}")),
                }
            } else if let Some(file) = &line.file {
                let r = Ref {
                    file: file.clone(),
                    key: line.key.clone(),
                    pattern: line.pattern.clone(),
                };
                files.insert(file.clone());
                match tree.resolve(&r, line.fields.as_deref()) {
                    Ok((source, value)) => {
                        if let Some(bad) = value
                            .texts()
                            .into_iter()
                            .find(|t| looks_like_key_material(t))
                        {
                            problems.push(format!(
                                "{at} ({source}) resolves to what looks like key material ({} chars) — the paper carries locations, never a value (design 125d405d Q1)",
                                bad.len()
                            ));
                        }
                        lines.push(LineOut::Fact {
                            label: line.label.clone().unwrap_or_default(),
                            source,
                            value,
                        });
                    }
                    Err(e) => problems.push(format!("{at}: {e}")),
                }
            }
        }
        let mut needs = Vec::new();
        if let Some(manifest) = road.needs.as_ref().and_then(|n| n.workload.as_ref()) {
            files.insert(manifest.clone());
            match derive_workload_needs(&mut tree, manifest) {
                Ok(n) => needs.push(n),
                Err(e) => problems.push(format!("road {:?} needs: {e}", road.id)),
            }
        }
        if let Some((n, host_ref)) = road
            .needs
            .as_ref()
            .and_then(|n| n.host.as_ref().map(|h| (n, h)))
        {
            let host = tree.resolve(host_ref, None);
            let path = n.path.as_ref().map(|p| tree.resolve(p, None)).transpose();
            match (host, path) {
                (Ok((hs, Value::Text(h))), Ok(p)) => {
                    files.insert(host_ref.file.clone());
                    let path_text = match &p {
                        Some((ps, Value::Text(t))) => {
                            files.insert(ps.split('#').next().unwrap_or_default().to_string());
                            t.clone()
                        }
                        _ => "/".to_string(),
                    };
                    match derive_needs(&mut tree, &host_of(&h), &path_text) {
                        Ok((derived, from)) => {
                            files.extend(from.into_iter().map(String::from));
                            needs.extend(derived);
                        }
                        Err(e) => {
                            problems.push(format!("road {:?} needs (host from {hs}): {e}", road.id))
                        }
                    }
                }
                (Err(e), _) | (_, Err(e)) => {
                    problems.push(format!("road {:?} needs: {e}", road.id))
                }
                (Ok((hs, _)), _) => problems.push(format!(
                    "road {:?} needs: {hs} is not a single host",
                    road.id
                )),
            }
        }
        roads.push(RoadOut {
            id: road.id.clone(),
            title: road.title.clone(),
            proven_by: road.proven_by.clone(),
            lines,
            needs,
            files,
        });
    }
    if problems.is_empty() {
        Ok(Rendered { roads })
    } else {
        Err(problems)
    }
}

impl Rendered {
    /// The canonical fact set the version is taken over: one line per
    /// printed thing, keyed by road and position, sorted.
    pub fn fact_set(&self) -> Vec<String> {
        let mut set = vec![format!("format {FORMAT}")];
        for (r, road) in self.roads.iter().enumerate() {
            let at = format!("{:02}-{}", r + 1, road.id);
            set.push(format!("{at}/title {}", road.title));
            set.push(format!(
                "{at}/proven_by {}",
                road.proven_by.as_deref().unwrap_or(NOT_EXERCISED)
            ));
            for (i, line) in road.lines.iter().enumerate() {
                set.push(match line {
                    LineOut::Fact {
                        label,
                        source,
                        value,
                    } => {
                        format!(
                            "{at}/{:02} fact {label} | {source} = {}",
                            i + 1,
                            value.canonical()
                        )
                    }
                    LineOut::Prose(p) => format!("{at}/{:02} prose {p}", i + 1),
                    LineOut::NotHeld(n) => format!("{at}/{:02} not_held {n}", i + 1),
                });
            }
            for (i, n) in road.needs.iter().enumerate() {
                set.push(format!("{at}/needs/{:02} {n}", i + 1));
            }
        }
        set.sort();
        set
    }

    /// The version: sha256 over the sorted fact set.
    pub fn hash(&self) -> String {
        reprint::hash_of(&self.fact_set())
    }

    /// The version as the page prints it: the hash's first
    /// [`VERSION_CHARS`].
    pub fn version(&self) -> String {
        reprint::version_of(&self.hash()).to_string()
    }

    pub fn facts_text(&self) -> String {
        let mut out = format!("recovery sheet version {}\n", self.version());
        for (r, road) in self.roads.iter().enumerate() {
            out.push_str(&format!("\n== {}. {} ==\n", r + 1, road.title));
            out.push_str(&format!(
                "   {}\n",
                road.proven_by
                    .as_ref()
                    .map(|p| format!("last exercised: packet {p}"))
                    .unwrap_or_else(|| NOT_EXERCISED.to_string())
            ));
            for line in &road.lines {
                match line {
                    LineOut::Fact {
                        label,
                        source,
                        value,
                    } => match value {
                        Value::Text(t) => {
                            out.push_str(&format!("   {label}: {t}\n      from {source}\n"))
                        }
                        Value::Items(items) => {
                            out.push_str(&format!("   {label}:\n      from {source}\n"));
                            for (n, item) in items.iter().enumerate() {
                                for (k, v) in item {
                                    out.push_str(&format!("      {}.{k}: {v}\n", n + 1));
                                }
                            }
                        }
                    },
                    LineOut::Prose(p) => out.push_str(&format!("   {p}\n")),
                    LineOut::NotHeld(n) => out.push_str(&format!("   {NOT_HELD}: {n}\n")),
                }
            }
            if !road.needs.is_empty() {
                out.push_str("   THIS ROAD NEEDS (derived):\n");
                for n in &road.needs {
                    out.push_str(&format!("      - {n}\n"));
                }
            }
            out.push_str(&format!(
                "   decided by: {}\n",
                road.files.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        out
    }

    /// Everything the page prints that `--facts` prints, road by road,
    /// each as the ONE contiguous string the page prints it as — a fact
    /// is `Label: value`, never the value alone, so a two-letter value
    /// cannot be found by coincidence elsewhere (review of car 2, F2).
    /// Each road's first entry is its heading, `N. title`, which is how
    /// the check finds the road's own stretch of the page. The version is
    /// not here: it is in the footer, which [`Rendered::judge_print`]
    /// holds to the whole stamp on every page.
    pub fn printed(&self) -> Vec<Vec<(String, String)>> {
        self.roads
            .iter()
            .enumerate()
            .map(|(r, road)| {
                let at = format!("road {} ({})", r + 1, road.id);
                let mut out = vec![
                    (format!("{at} title"), format!("{}. {}", r + 1, road.title)),
                    (
                        format!("{at} proof"),
                        road.proven_by
                            .as_ref()
                            .map(|p| format!("Last exercised: packet {p}"))
                            .unwrap_or_else(|| NOT_EXERCISED.to_string()),
                    ),
                ];
                for (i, line) in road.lines.iter().enumerate() {
                    let at = format!("{at} line {}", i + 1);
                    match line {
                        LineOut::Fact {
                            label,
                            value: Value::Text(t),
                            ..
                        } => out.push((format!("{at} fact ({label})"), format!("{label}: {t}"))),
                        LineOut::Fact {
                            label,
                            value: Value::Items(items),
                            ..
                        } => {
                            out.push((format!("{at} fact ({label})"), format!("{label}:")));
                            out.extend(items.iter().enumerate().map(|(n, item)| {
                                (
                                    format!("{at} fact ({label}) item {}", n + 1),
                                    item.iter().map(|(_, v)| v.as_str()).collect::<String>(),
                                )
                            }));
                        }
                        LineOut::Prose(p) => out.push((format!("{at} prose"), p.clone())),
                        LineOut::NotHeld(n) => {
                            out.push((format!("{at} not held"), format!("{NOT_HELD}: {n}")))
                        }
                    }
                }
                out.extend(
                    road.needs
                        .iter()
                        .enumerate()
                        .map(|(i, n)| (format!("{at} need {}", i + 1), n.clone())),
                );
                out.push((
                    format!("{at} deciding files"),
                    format!(
                        "Decided by: {}",
                        road.files.iter().cloned().collect::<Vec<_>>().join(", ")
                    ),
                ));
                out
            })
            .collect()
    }

    /// Where the sheet says something `body` does not. Each road's
    /// entries are looked for only in that road's own stretch — from its
    /// heading to the next road's — and compared with every whitespace
    /// removed from both sides, because a printed page wraps a line
    /// wherever the paper ends.
    pub fn missing_from(&self, body: &str) -> Vec<String> {
        let page = squash(body);
        let roads = self.printed();
        // Each heading's position, found in order after the one before.
        let mut starts: Vec<Option<usize>> = Vec::new();
        let mut from = 0;
        for road in &roads {
            let heading = road.first().map(|(_, h)| squash(h)).unwrap_or_default();
            let found = page
                .get(from..)
                .and_then(|rest| rest.find(&heading))
                .map(|i| from + i);
            if let Some(i) = found {
                from = i + heading.len();
            }
            starts.push(found);
        }
        let mut missing = Vec::new();
        for (r, road) in roads.iter().enumerate() {
            let Some(start) = starts[r] else {
                missing.push(format!(
                    "{} — the road's heading, so none of its lines can be placed",
                    road.first().map(|(at, _)| at.as_str()).unwrap_or("a road")
                ));
                continue;
            };
            let end = starts[r + 1..]
                .iter()
                .flatten()
                .next()
                .copied()
                .unwrap_or(page.len());
            let section = &page[start..end];
            missing.extend(
                road.iter()
                    .filter(|(_, t)| !section.contains(&squash(t)))
                    .map(|(at, _)| at.clone()),
            );
        }
        missing
    }

    /// Why a printed PDF, read back as `runs`, may not be handed over —
    /// empty when it may. Two things, both about the PAPER rather than
    /// the file (review of car 2, F1, F3):
    ///   - every page's bottom band ([`FOOTER_BAND_PT`], the page margin
    ///     the footer prints in) holds exactly `footer` — the version, the
    ///     tree and the render time the page was printed with — and
    ///     nothing else, so no body text prints over or under it;
    ///   - the body above the band says every entry [`Rendered::printed`]
    ///     lists, each in its own road's stretch.
    pub fn judge_print(&self, runs: &[pdf::Run], footer: &str) -> Vec<String> {
        let mut problems = Vec::new();
        let pages = runs.iter().map(|r| r.page + 1).max().unwrap_or(0);
        if pages == 0 {
            problems.push("the PDF shows no text at all".to_string());
        }
        for page in 1..=pages {
            let band: Vec<pdf::Run> = runs
                .iter()
                .filter(|r| r.page + 1 == page && r.y < FOOTER_BAND_PT)
                .cloned()
                .collect();
            if squash(&pdf::join(&band)) != squash(footer) {
                problems.push(format!(
                    "page {page}: the bottom band does not hold exactly the footer this sheet was printed with (a different stamp, a missing footer, or body text printed into it)"
                ));
            }
        }
        let body: Vec<pdf::Run> = runs
            .iter()
            .filter(|r| r.y >= FOOTER_BAND_PT)
            .cloned()
            .collect();
        problems.extend(self.missing_from(&pdf::join(&body)));
        problems
    }

    /// The page printed to PDF, and read back: refused unless
    /// [`Rendered::judge_print`] finds nothing. The print and its proof
    /// are one act.
    pub fn print_pdf(
        &self,
        tree_sha: &str,
        rendered_at: &str,
        chromium: Option<&Path>,
    ) -> Result<Printed, String> {
        let chromium = pdf::find_chromium(chromium)?;
        let bytes = pdf::print(&chromium, &self.html(tree_sha, rendered_at))?;
        let runs = pdf::runs(&bytes).map_err(|e| format!("the PDF could not be read back: {e}"))?;
        let problems = self.judge_print(&runs, &self.footer(tree_sha, rendered_at));
        if !problems.is_empty() {
            return Err(format!(
                "the PDF {} printed is not the sheet ({} problem(s)), so it is not handed over: {}",
                chromium.display(),
                problems.len(),
                problems.join("; ")
            ));
        }
        Ok(Printed {
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes,
        })
    }

    /// The footer every printed page carries: the version, the tree it
    /// was rendered from, and when.
    pub fn footer(&self, tree_sha: &str, rendered_at: &str) -> String {
        format!(
            "version {} · tree {tree_sha} · rendered {rendered_at}",
            self.version()
        )
    }

    /// The printable page: black on white, large type, one section per
    /// road, and the footer ([`Rendered::footer`]) printed in the bottom
    /// page margin of every page — a margin box, so the page reserves
    /// its band and no body text can reach it.
    pub fn html(&self, tree_sha: &str, rendered_at: &str) -> String {
        let version = self.version();
        let stamp = css_string(&self.footer(tree_sha, rendered_at));
        let band = FOOTER_BAND_PT;
        let mut body = String::new();
        for (r, road) in self.roads.iter().enumerate() {
            body.push_str(&format!(
                "<section><h2>{}. {}</h2>\n<p class=\"proof\">{}</p>\n<ul>\n",
                r + 1,
                esc(&road.title),
                road.proven_by
                    .as_ref()
                    .map(|p| format!("Last exercised: packet {}", esc(p)))
                    .unwrap_or_else(|| NOT_EXERCISED.to_string())
            ));
            for line in &road.lines {
                match line {
                    LineOut::Fact {
                        label,
                        value: Value::Text(t),
                        ..
                    } => body.push_str(&format!(
                        "<li class=\"fact\">{}: <code>{}</code></li>\n",
                        esc(label),
                        esc(t)
                    )),
                    LineOut::Fact {
                        label,
                        value: Value::Items(items),
                        ..
                    } => {
                        body.push_str(&format!("<li class=\"fact\">{}:<ol>\n", esc(label)));
                        for item in items {
                            body.push_str("<li>");
                            for (k, v) in item {
                                if k == "command" {
                                    body.push_str(&format!("<pre>{}</pre>", esc(v)));
                                } else {
                                    body.push_str(&format!(
                                        "<span class=\"{}\">{}</span> ",
                                        esc(k),
                                        esc(v)
                                    ));
                                }
                            }
                            body.push_str("</li>\n");
                        }
                        body.push_str("</ol></li>\n");
                    }
                    LineOut::Prose(p) => {
                        body.push_str(&format!("<li class=\"prose\">{}</li>\n", esc(p)))
                    }
                    LineOut::NotHeld(n) => body.push_str(&format!(
                        "<li class=\"held\"><strong>{NOT_HELD}:</strong> {}</li>\n",
                        esc(n)
                    )),
                }
            }
            body.push_str("</ul>\n");
            if !road.needs.is_empty() {
                body.push_str("<p class=\"needs\"><strong>THIS ROAD NEEDS (derived):</strong></p><ul class=\"needs\">\n");
                for n in &road.needs {
                    body.push_str(&format!("<li>{}</li>\n", esc(n)));
                }
                body.push_str("</ul>\n");
            }
            body.push_str(&format!(
                "<p class=\"files\">Decided by: {}</p></section>\n",
                road.files
                    .iter()
                    .map(|f| esc(f))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        format!(
            "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n\
<title>Recovery sheet {version}</title>\n<style>\n\
@page {{ size: letter; margin: 0.5in 0.6in {band}pt; \
@bottom-left {{ content: \"{stamp}\"; font-family: Helvetica, Arial, sans-serif; font-size: 9pt; color: #000; vertical-align: middle; border-top: 1px solid #000; }} }}\n\
body {{ font-family: Helvetica, Arial, sans-serif; font-size: 14pt; line-height: 1.35; color: #000; background: #fff; margin: 0; }}\n\
h1 {{ font-size: 24pt; margin: 0 0 4pt; }}\n\
h2 {{ font-size: 18pt; border-top: 3px solid #000; padding-top: 6pt; margin: 18pt 0 4pt; }}\n\
section {{ break-inside: avoid; }}\n\
code, pre {{ font-family: 'DejaVu Sans Mono', Menlo, monospace; font-weight: bold; }}\n\
pre {{ white-space: pre-wrap; word-break: break-all; border: 1px solid #000; padding: 4pt; margin: 4pt 0; font-size: 12pt; }}\n\
.proof, .files {{ font-size: 11pt; }}\n\
.held {{ border-left: 6px solid #000; padding-left: 6pt; }}\n\
</style></head><body>\n\
<h1>Getting back in — the recovery sheet</h1>\n\
<p>Roads in the order to try them. Every address, path and name below was read from the file named under its road; nothing was typed. No PIN, password or key belongs on this page: the renderer refuses the shapes it can recognise, and a review holds the rest.</p>\n\
{body}</body></html>\n"
        )
    }
}

/// The bottom page margin, in points, that the footer prints in and no
/// body text may enter — one number for the page's CSS and for the
/// read-back's band, so the two cannot disagree (review of car 2, F1:
/// a `position: fixed` footer had no margin of its own on a page after
/// the first, and printed over road 1's and road 7's continuations).
pub const FOOTER_BAND_PT: f64 = 72.0;

/// A text with every whitespace removed: a printed page breaks lines
/// wherever the paper ends, so words are compared without them.
fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// A CSS string literal's contents: the footer is generated content in
/// the page margin (`@page { @bottom-left { content: "…" } }`), so it is
/// escaped for CSS rather than for HTML — an entity there would print
/// as written. `<` is escaped too, so no value can close the style tag.
fn css_string(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\\' => "\\\\".to_string(),
            '"' => "\\\"".to_string(),
            '<' => "\\3C ".to_string(),
            c if c.is_control() => format!("\\{:X} ", u32::from(c)),
            c => c.to_string(),
        })
        .collect()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_testing::scratch::{scratch_dir, write_file};

    fn tree_sheet() -> Rendered {
        match load(&boss_testing::repo_root()) {
            Ok(r) => r,
            Err(problems) => panic!(
                "the tree's recovery sheet must render:\n  {}",
                problems.join("\n  ")
            ),
        }
    }

    fn road<'a>(r: &'a Rendered, id: &str) -> &'a RoadOut {
        r.roads
            .iter()
            .find(|x| x.id == id)
            .unwrap_or_else(|| panic!("road {id}"))
    }

    fn fact<'a>(road: &'a RoadOut, label_start: &str) -> &'a Value {
        road.lines
            .iter()
            .find_map(|l| match l {
                LineOut::Fact { label, value, .. } if label.starts_with(label_start) => Some(value),
                _ => None,
            })
            .unwrap_or_else(|| panic!("fact {label_start:?} on road {}", road.id))
    }

    /// THE PIN (design 125d405d §2): every reference in the tree's
    /// source resolves to exactly one value, every prose line is free of
    /// literals, every road's dependencies derive — so a car that moves a
    /// key the sheet reads fails its own gate instead of staling the paper.
    #[test]
    fn the_recovery_sheet_resolves_every_fact() {
        let r = tree_sheet();
        assert!(r.roads.len() >= 10, "every road back in: {}", r.roads.len());
        // Read, not retyped: the values come out of the files that decide
        // them. Spot-checked against the file's own consumer below.
        let door = road(&r, "dev-door-anywhere");
        let Value::Items(steps) = fact(door, "Client setup") else {
            panic!("the steps are a list")
        };
        assert_eq!(
            steps.len(),
            3,
            "the page's three steps, from the one data file"
        );
        assert!(
            steps
                .iter()
                .all(|s| s.iter().all(|(_, v)| !v.contains("{host}"))),
            "{{host}} is filled with the file's own host, as the page fills it"
        );
        assert_eq!(
            fact(road(&r, "durable-session"), "tmux session"),
            &Value::Text("dev".into())
        );
        // No evidence is not a pass: every road without a proving packet
        // says so on the paper.
        let text = r.facts_text();
        for road in r.roads.iter().filter(|x| x.proven_by.is_none()) {
            let section = text
                .split("\n== ")
                .find(|s| s.contains(&road.title))
                .unwrap_or_default();
            assert!(
                section.contains(NOT_EXERCISED),
                "road {} must print {NOT_EXERCISED}",
                road.id
            );
        }
        // And a road the record HAS proven says so (review F4: both keys
        // opened the door, 2026-09-29 01:36Z and 01:37Z, design c5ce1aeb).
        assert_eq!(
            road(&r, "break-glass").proven_by.as_deref(),
            Some("c5ce1aeb")
        );
        assert!(text.contains("last exercised: packet c5ce1aeb"), "{text}");
    }

    /// Review F2/F3: the roads whose only copy was in the two runbooks
    /// this car deletes are on the sheet, whole, and read from the tree.
    #[test]
    fn the_roads_the_runbooks_held_are_on_the_sheet() {
        let r = tree_sheet();
        let admin = road(&r, "cluster-admin");
        // The WHOLE runnable line (round-2 R2-2): the forge has no host
        // kubectl and the kubeconfig is root's, so it is the docker line
        // the forge's own loops run, with the image read from the tree.
        let Value::Text(exec) = fact(
            admin,
            "On the forge, open the workspace when both ssh doors are dark",
        ) else {
            panic!("a command is one line")
        };
        assert_eq!(
            exec,
            "sudo docker run --rm -it --network host -v /etc/boss-ops/kubeconfig:/kc:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc -n boss-dev exec -it deploy/boss-dev -c dev -- /work/dev-session.sh"
        );
        // R2-1: the admin credential is the forge's alone; the bastion's
        // is the scoped one, at the same path, on the next road.
        assert_eq!(fact(admin, "On the host"), &Value::Text("forge".into()));
        let scoped = road(&r, "bastion-cluster-credential");
        assert_eq!(fact(scoped, "On the host"), &Value::Text("boss-gcp".into()));
        assert_eq!(
            fact(scoped, "At the same path"),
            &Value::Text("/etc/boss-ops".into())
        );
        let Value::Text(jump) = fact(admin, "Reach the forge through the bastion") else {
            panic!("a command is one line")
        };
        let forge = fact(road(&r, "forge-and-record"), "Forge: git");
        let Value::Text(forge_url) = forge else {
            panic!()
        };
        let forge_host = host_of(forge_url);
        assert!(
            jump.starts_with("ssh -J ") && jump.ends_with(&format!("@{forge_host}")),
            "{jump}"
        );
        let backups = road(&r, "restore-the-record");
        assert_eq!(
            fact(backups, "Into the directory"),
            &Value::Text("/var/backups/boss-cluster-pg".into())
        );
        assert_eq!(
            fact(backups, "Offsite dumps kept"),
            &Value::Text("21".into())
        );
        assert_eq!(
            fact(backups, "In-cluster copies kept"),
            &Value::Text("14".into())
        );
    }

    /// Review F8: the workspace roads say the cluster must be up, and the
    /// bastion comes before the door that is reached through it.
    #[test]
    fn a_road_comes_after_the_road_it_needs() {
        let r = tree_sheet();
        for id in ["durable-session", "dev-door-lan"] {
            let needs = road(&r, id).needs.join("\n");
            assert!(
                needs.contains("the cluster up") && needs.contains("boss-dev"),
                "{id}: {needs}"
            );
        }
        let at = |id: &str| r.roads.iter().position(|x| x.id == id).unwrap();
        assert!(at("bastion") < at("dev-door-lan"));
        assert!(at("cluster-admin") < at("bastion-cluster-credential"));
    }

    /// Section 5: the break-glass road's dependencies are DERIVED — the
    /// Access application on the whole host, the in-cluster connector,
    /// the plain-HTTP LAN address — and the dev door's too.
    #[test]
    fn the_break_glass_road_says_what_it_needs_and_what_it_does_not_cover() {
        let r = tree_sheet();
        let bg = road(&r, "break-glass");
        let needs = bg.needs.join("\n");
        assert!(
            needs.contains("one-time code") && needs.contains("WHOLE host"),
            "{needs}"
        );
        assert!(needs.contains("INSIDE the cluster"), "{needs}");
        assert!(
            needs.contains("plain HTTP") && needs.contains("no LAN road"),
            "{needs}"
        );
        assert!(
            bg.files.contains(ACCESS) && bg.files.contains(CONNECTOR),
            "{:?}",
            bg.files
        );
        let dev = road(&r, "dev-door-anywhere").needs.join("\n");
        assert!(
            dev.contains("certificate") && dev.contains("INSIDE the cluster"),
            "{dev}"
        );
        assert!(
            !dev.contains("plain HTTP"),
            "the dev door is not an instance's gateway: {dev}"
        );
    }

    /// Backlog 41c5ddaf (DR readiness 62dac114 item 4, David 2026-09-29):
    /// the second person is on the paper as the ROLE "Apple Passkey
    /// Delegate", never by name. The section says the delegate takes the
    /// ordinary edge road and never break-glass, what they can and
    /// cannot do, and reads who the edge admits rather than asserting it.
    #[test]
    fn the_apple_passkey_delegate_is_on_the_sheet() {
        let r = tree_sheet();
        let d = road(&r, "apple-passkey-delegate");
        assert!(d.title.contains("Apple Passkey Delegate"), "{}", d.title);
        let prose: Vec<&str> = d
            .lines
            .iter()
            .filter_map(|l| match l {
                LineOut::Prose(p) => Some(p.as_str()),
                _ => None,
            })
            .collect();
        let all = prose.join("\n");
        assert!(all.contains("ordinary edge road"), "{all}");
        assert!(all.contains("never the break-glass"), "{all}");
        assert!(all.contains("CAN:") && all.contains("CANNOT:"), "{all}");
        assert!(
            matches!(
                fact(d, "Identity the edge admits"),
                Value::Text(_) | Value::Items(_)
            ),
            "the delegate's road is read from the edge policy"
        );
        let at = |id: &str| r.roads.iter().position(|x| x.id == id).unwrap();
        assert!(at("break-glass") < at("apple-passkey-delegate"));
        assert!(
            r.html("sha", "now").contains("Apple Passkey Delegate"),
            "the printed sheet shows the section"
        );
    }

    /// The estate's flat keys are read by the SHELL on every host
    /// (render-sor-env.sh). This reader must answer what that one does,
    /// or the paper and the hosts would disagree about the same line.
    #[test]
    fn the_estate_keys_read_here_equal_what_the_hosts_render() {
        let root = boss_testing::repo_root();
        let r = tree_sheet();
        let forge = road(&r, "forge-and-record");
        for (label, rendered_key) in [
            ("Forge: git", "BOSS_FORGE_URL"),
            ("Forge journal", "BOSS_FORGE_JOURNAL_URL"),
            ("System of record", "BOSS_JOBS_URL"),
            ("Public mirror", "BOSS_MIRROR_URL"),
        ] {
            let out = std::process::Command::new("bash")
                .arg(root.join("infra/estate/render-sor-env.sh"))
                .args(["--value", rendered_key])
                .output()
                .expect("bash runs");
            assert!(
                out.status.success(),
                "render-sor-env.sh --value {rendered_key}"
            );
            let shell = String::from_utf8_lossy(&out.stdout).trim().to_string();
            assert_eq!(
                fact(forge, label),
                &Value::Text(shell),
                "{label} vs {rendered_key}"
            );
        }
    }

    /// Copy the files the tree's sheet reads into a scratch tree.
    fn copied_tree(name: &str) -> PathBuf {
        let root = boss_testing::repo_root();
        let dir = scratch_dir(name);
        let r = tree_sheet();
        let files: BTreeSet<String> = r
            .roads
            .iter()
            .flat_map(|x| x.files.iter().cloned())
            .collect();
        for f in files {
            let to = dir.join(&f);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(root.join(&f), &to).unwrap_or_else(|e| panic!("copy {f}: {e}"));
        }
        dir
    }

    #[test]
    fn a_moved_key_fails_the_sheet_by_name() {
        let dir = copied_tree("recovery-moved-key");
        assert!(load(&dir).is_ok(), "the copy renders as the tree does");
        let estate = dir.join(ESTATE);
        let text = std::fs::read_to_string(&estate).unwrap();
        write_file(
            &estate,
            &text.replace("forge_journal = ", "forge_journal_door = "),
        );
        let problems = load(&dir).expect_err("a moved key must fail the sheet");
        assert!(
            problems
                .iter()
                .any(|p| p.contains("infra/estate/estate.toml#forge_journal")),
            "the problem names the file and key: {problems:?}"
        );
        // A pattern whose line moved fails the same way.
        let dir = copied_tree("recovery-moved-line");
        let unit = dir.join("infra/forge/cluster-watchdog.timer");
        let text = std::fs::read_to_string(&unit).unwrap();
        write_file(&unit, &text.replace("OnUnitActiveSec=", "OnActiveSec="));
        let problems = load(&dir).expect_err("a moved line must fail the sheet");
        assert!(
            problems
                .iter()
                .any(|p| p.contains("cluster-watchdog.timer") && p.contains("matches nothing")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_file_its_consumer_refuses_fails_the_sheet() {
        let dir = copied_tree("recovery-consumer-refuses");
        let access = dir.join(ACCESS);
        let text = std::fs::read_to_string(&access).unwrap();
        write_file(
            &access,
            &format!("{text}\n[[application]]\nname = \"typo\"\ndomian = \"x\"\n"),
        );
        let problems = load(&dir).expect_err("the observer's parse refuses an unknown key");
        assert!(
            problems.iter().any(|p| p.contains("dns observer refuses")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_prose_line_carrying_a_literal_is_refused() {
        for literal in [
            "ssh to 192.0.2.7 first",
            "open https://example.test now",
            "the host door.example.test answers",
            "read /etc/boss-ops by hand",
            "it listens on :7900",
            "mail someone@example.test",
            "edit boss.yaml",
        ] {
            let src = format!(
                "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nprose = \"{literal}\"\n"
            );
            let problems = parse_sheet(&src).expect_err(literal);
            assert!(problems[0].contains("FACT line"), "{literal}: {problems:?}");
        }
        let ok = "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nprose = \"Detach with ctrl-b then d; see backlog c1bb822e, e.g. the kit.\"\n";
        assert!(parse_sheet(ok).is_ok(), "procedure words pass");
    }

    #[test]
    fn a_line_is_exactly_one_kind() {
        let both = "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nprose = \"a\"\nnot_tree_held = \"b\"\n";
        assert!(parse_sheet(both).unwrap_err()[0].contains("exactly one"));
        let unlabelled =
            "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nfile = \"f.toml\"\nkey = \"k\"\n";
        assert!(parse_sheet(unlabelled).unwrap_err()[0].contains("label"));
        let unknown = "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nvalue = \"typed\"\n";
        assert!(
            parse_sheet(unknown).is_err(),
            "a key the shape does not name is refused"
        );
    }

    #[test]
    fn key_paths_select_one_many_or_every() {
        let doc = serde_json::json!({
            "node": [
                {"id": "a", "address": "192.0.2.1", "roles": ["x", "y"]},
                {"id": "b", "address": "192.0.2.2", "roles": ["y"]},
            ],
            "flat": "v"
        });
        let text = |p: &str| printable("x.json", &doc, &walk(&doc, p).unwrap(), None).unwrap();
        assert_eq!(text("flat"), Value::Text("v".into()));
        assert_eq!(text("node[id=b].address"), Value::Text("192.0.2.2".into()));
        assert_eq!(text("node[roles~y].id"), Value::Text("a, b".into()));
        assert_eq!(text("node[roles~x].id"), Value::Text("a".into()));
        assert_eq!(text("node.id"), Value::Text("a, b".into()));
        assert!(
            walk(&doc, "node[id=c].address")
                .unwrap_err()
                .contains("no node")
        );
        assert!(walk(&doc, "node[roles~z].id").is_err());
        assert!(walk(&doc, "missing").unwrap_err().contains("no key"));
        let twin = serde_json::json!({"t": [{"n": "a"}, {"n": "a"}]});
        assert!(walk(&twin, "t[n=a].n").unwrap_err().contains("exactly one"));
    }

    #[test]
    fn a_pattern_must_capture_one_value() {
        assert_eq!(capture("A=1\nA=1\n", r"^A=(\d)$").unwrap(), "1");
        assert!(
            capture("A=1\nA=2\n", r"^A=(\d)$")
                .unwrap_err()
                .contains("different values")
        );
        assert!(
            capture("B=1\n", r"^A=(\d)$")
                .unwrap_err()
                .contains("matches nothing")
        );
        assert!(
            capture("A=1\n", r"^(A)=(\d)$")
                .unwrap_err()
                .contains("one capture group")
        );
    }

    /// Review F7: a refusal names LINES, never the values it refused —
    /// what a pattern captured may be exactly what must not reach a log.
    #[test]
    fn a_refusal_never_prints_what_it_read() {
        let one = format!("AAAA{}", "secretOne");
        let two = format!("BBBB{}", "secretTwo");
        let why = capture(&format!("x\nV={one}\ny\nV={two}\n"), r"^V=(\S+)$").unwrap_err();
        assert!(!why.contains(&one) && !why.contains(&two), "{why}");
        assert!(
            why.contains("2 different values") && why.contains("[2, 4]"),
            "{why}"
        );
        // A parser's error quotes the offending line; the sheet's does not.
        let hidden = format!("value = {}", "unquoted-sekrit");
        let why = data_file("infra/x.toml", &format!("a = 1\n{hidden}\n")).unwrap_err();
        assert!(!why.contains("sekrit"), "{why}");
        assert!(why.starts_with("infra/x.toml: "), "{why}");
    }

    /// Review F1: EVERY string the source carries is judged for the
    /// shapes of a secret — one case per kind of string, each with the
    /// four shapes the review injected.
    #[test]
    fn every_string_in_the_source_is_judged_for_a_secret() {
        // Assembled, so the tree's own no-secrets lint does not read the
        // fixture as a key.
        let ssh = format!("ssh-ed25519 AAAA{}", "C3NzaC1lZDI1NTE5");
        let shapes = [
            ssh.as_str(),
            "the kit PIN is 4821",
            "password: hunter2",
            "the YubiKey on the blue keychain opens the LAN door",
        ];
        let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        for shape in shapes {
            let s = q(shape);
            let cases = [
                (
                    "title",
                    format!(
                        "[[road]]\nid = \"r\"\ntitle = \"{s}\"\n[[road.line]]\nprose = \"a\"\n"
                    ),
                ),
                (
                    "label",
                    format!(
                        "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nlabel = \"{s}\"\nfile = \"f.toml\"\nkey = \"k\"\n"
                    ),
                ),
                (
                    "prose",
                    format!(
                        "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nprose = \"{s}\"\n"
                    ),
                ),
                (
                    "not_tree_held",
                    format!(
                        "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nnot_tree_held = \"{s}\"\n"
                    ),
                ),
                (
                    "pattern",
                    format!(
                        "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nlabel = \"l\"\nfile = \"f.sh\"\npattern = \"{s} (x)\"\n"
                    ),
                ),
                (
                    "key",
                    format!(
                        "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nlabel = \"l\"\nfile = \"f.toml\"\nkey = \"{s}\"\n"
                    ),
                ),
                (
                    "command",
                    format!(
                        "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nlabel = \"l\"\ncommand = \"{s} {{v}}\"\nwith = {{ v = {{ file = \"f.toml\", key = \"k\" }} }}\n"
                    ),
                ),
            ];
            for (kind, src) in cases {
                let problems = parse_sheet(&src).expect_err(&format!("{kind}: {shape}"));
                assert!(
                    problems.iter().any(|p| p.contains(NEVER_ON_PAPER)),
                    "{kind} carrying {shape:?} must be refused as a secret: {problems:?}"
                );
                assert!(
                    problems.iter().all(|p| !p.contains(shape)),
                    "the refusal names the kind of secret, never the secret"
                );
            }
        }
        // The words the tree's own sheet uses pass: naming what is NOT
        // held is not declaring it.
        for fine in [
            "which private key opens this door, and which device holds it. Never declared.",
            "The kit is a PIN-protected stick, and the PIN never appears here.",
            "Touch either hardware key, primary or backup.",
        ] {
            assert_eq!(secret_in(fine), None, "{fine}");
        }
    }

    #[test]
    fn a_command_is_filled_only_from_facts_it_names() {
        let unnamed = "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nlabel = \"l\"\ncommand = \"ssh {user}@{host}\"\nwith = { user = { file = \"f.toml\", key = \"u\" } }\n";
        assert!(parse_sheet(unnamed).unwrap_err()[0].contains("placeholders"));
        let literal = "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nlabel = \"l\"\ncommand = \"ssh {user}@192.0.2.7\"\nwith = { user = { file = \"f.toml\", key = \"u\" } }\n";
        assert!(parse_sheet(literal).unwrap_err()[0].contains("FACT line"));
        let ok = "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nlabel = \"l\"\ncommand = \"ssh {user}@{host}\"\nwith = { user = { file = \"f.toml\", key = \"u\" }, host = { file = \"f.toml\", key = \"h\" } }\n";
        let sheet = parse_sheet(ok).unwrap();
        let dir = scratch_dir("recovery-command");
        write_file(&dir.join("f.toml"), "u = \"op\"\nh = \"door\"\n");
        let r = resolve(&dir, &sheet).unwrap();
        assert_eq!(
            r.roads[0].lines[0],
            LineOut::Fact {
                label: "l".into(),
                source: "user=f.toml#u; host=f.toml#h".into(),
                value: Value::Text("ssh op@door".into()),
            }
        );
    }

    #[test]
    fn a_value_that_looks_like_key_material_is_refused() {
        // Assembled, so the tree's own no-secrets lint does not read
        // this fixture as a key.
        let armour = format!("-----BEGIN {} KEY-----", "OPENSSH PRIVATE");
        assert!(looks_like_key_material(&armour));
        assert!(looks_like_key_material(&"Zm9v".repeat(12)));
        assert!(!looks_like_key_material("dev.algedonic.dev"));
        assert!(!looks_like_key_material(
            "grep -qsF 'Match host h ' ~/.ssh/config || cloudflared access ssh-config --hostname h"
        ));
        let dir = copied_tree("recovery-key-material");
        let estate = dir.join(ESTATE);
        let text = std::fs::read_to_string(&estate).unwrap();
        let material = "Zm9vYmFyZm9vYmFyZm9vYmFyZm9vYmFyZm9vYmFyZm9vYmFy";
        write_file(
            &estate,
            &text.replace("mirror_url = \"", &format!("mirror_url = \"{material}")),
        );
        let problems = load(&dir).expect_err("key material never reaches the paper");
        assert!(
            problems.iter().any(|p| p.contains("key material")),
            "{problems:?}"
        );
        assert!(
            problems.iter().all(|p| !p.contains(material)),
            "the refusal does not print the value"
        );
    }

    /// Section 3: the version is over the FACTS — a second render at
    /// another time has the same version, a changed fact a new one, and
    /// the page carries its version in the footer that prints on every
    /// page.
    #[test]
    fn the_version_is_over_the_facts_never_the_render() {
        let a = tree_sheet();
        let b = tree_sheet();
        assert_eq!(a.hash(), b.hash());
        assert_eq!(a.hash().len(), 64);
        let early = a.html("abc", "2026-09-29 00:00 UTC");
        let late = a.html("abc", "2026-09-30 00:00 UTC");
        assert_ne!(early, late);
        assert!(early.contains(&format!("version {}", &a.hash()[..12])));
        assert!(
            early.contains("@bottom-left"),
            "the footer is a page-margin box"
        );

        let dir = copied_tree("recovery-hash-moves");
        let before = load(&dir).unwrap().hash();
        let timer = dir.join("infra/forge/cluster-watchdog.timer");
        let text = std::fs::read_to_string(&timer).unwrap();
        write_file(
            &timer,
            &text.replace("OnUnitActiveSec=5min", "OnUnitActiveSec=7min"),
        );
        assert_ne!(
            load(&dir).unwrap().hash(),
            before,
            "a changed fact is a new version"
        );
    }

    /// A one-line sheet reading `f.<ext>` by key or pattern, resolved over
    /// a scratch tree holding `body` at that file.
    fn one_fact(name: &str, file: &str, read: &str, body: &str) -> Result<Rendered, Vec<String>> {
        let src = format!(
            "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nlabel = \"l\"\nfile = \"{file}\"\n{read}\n"
        );
        let sheet = parse_sheet(&src)?;
        let dir = scratch_dir(name);
        write_file(&dir.join(file), body);
        resolve(&dir, &sheet)
    }

    /// Round-2 R2-3: a fact is refused by the NAME of the key it reads,
    /// by the file declaring a Secret, and by the shape of what it read —
    /// the prose rules, less the bare number, which is where a port lives.
    #[test]
    fn a_fact_that_reads_a_secret_is_refused_three_ways() {
        for key in [
            "pin",
            "admin.forge_token",
            "db[name=x].password",
            "totp-seed",
        ] {
            let src = format!(
                "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nlabel = \"l\"\nfile = \"f.toml\"\nkey = \"{key}\"\n"
            );
            let problems = parse_sheet(&src).expect_err(key);
            assert!(
                problems
                    .iter()
                    .any(|p| p.contains("name says it holds a secret")),
                "{key}: {problems:?}"
            );
        }
        for fine in [
            "host",
            "forge_url",
            "node[id=boss-gcp].address",
            "pinned_image",
        ] {
            assert!(!secret_key(fine), "{fine}");
        }

        let manifest = "kind: ServiceAccount\nmetadata:\n  name: op\n---\nkind: Secret\nmetadata:\n  name: op-token\n";
        let problems = one_fact(
            "recovery-secret-kind",
            "m.yaml",
            "pattern = '^  name: (op)$'",
            manifest,
        )
        .expect_err("a file declaring a Secret is never read");
        assert!(
            problems.iter().any(|p| p.contains("kind: Secret")),
            "{problems:?}"
        );
        let bare = one_fact("recovery-secret-kind-path", "m.yaml", "", manifest)
            .expect("naming the file prints only its path");
        assert_eq!(
            bare.roads[0].lines[0],
            LineOut::Fact {
                label: "l".into(),
                source: "m.yaml".into(),
                value: Value::Text("m.yaml".into()),
            }
        );

        let stated = one_fact(
            "recovery-stated-value",
            "f.toml",
            "key = \"note\"",
            "note = \"password: hunter2\"\n",
        )
        .expect_err("a stated secret in a value");
        assert!(
            stated.iter().any(|p| p.contains("a secret stated")),
            "{stated:?}"
        );
        assert!(
            stated.iter().all(|p| !p.contains("hunter2")),
            "never the value"
        );
        let port = one_fact(
            "recovery-port-value",
            "f.toml",
            "key = \"port\"",
            "port = \"51820\"\n",
        )
        .expect("a port is a value, not a PIN");
        assert_eq!(fact(&port.roads[0], "l"), &Value::Text("51820".into()));
    }

    /// Round-3 R3-2: the words that name a secret are wider, and four of
    /// them — `code`, `pass`, `key`, `auth` — are also ordinary words in
    /// the name of a LOCATION (`auth_provider`, `key_id`, `code_dir`), so
    /// those four name a secret as the word a name ENDS in, or just before
    /// a last word that carries a value. Review of car 2, F4: camelCase
    /// is split into the same words first, so one rule judges every
    /// spelling, and `cookie`, `jwt` and the value-carrying ends join.
    #[test]
    fn a_secret_is_named_by_more_words_than_it_was() {
        for key in [
            "forgeTokenFile",
            "forge_token_file",
            "adminPasswordValue",
            "apiKeyValue",
            "authHeader",
            "pass_phrase",
            "KEY_PEM",
            "key_material",
            "signing-key-data",
            "webhookSecretRef",
            "passwordHash",
            "session_cookie",
            "jwt",
            "AUTHORIZATION",
            "APIKey",
            "recoveryCodes",
        ] {
            assert!(secret_key(key), "{key} names a secret (F4)");
        }
        for fine in [
            "pass_file",
            "key_path",
            "ssh_key_file",
            "sshKeyFile",
            "BOSS_AUTH_PROVIDER",
            "token_id",
            "loadBalancerIP",
            "OnUnitActiveSec",
        ] {
            assert!(!secret_key(fine), "{fine} names a location (F4)");
        }
        assert_eq!(name_words("APIKeyValue"), ["api", "key", "value"]);
        assert_eq!(
            name_words("forge_token-File2x"),
            ["forge", "token", "file2x"]
        );
        for key in [
            "ssh.key",
            "db.pass",
            "admin_pwd",
            "x.pw",
            "bearer",
            "basic_auth",
            "github_creds",
            "recovery_code",
            "deploy-keys",
            "apiKey",
            "adminPassword",
            "bearer_header",
        ] {
            assert!(secret_key(key), "{key} names a secret");
        }
        for fine in [
            "host",
            "forge_url",
            "node[id=boss-gcp].address",
            "pinned_image",
            "auth_provider",
            "key_id",
            "code_dir",
            "secretName",
            "passage",
            "keyboard",
            "authority",
            "session_duration",
        ] {
            assert!(!secret_key(fine), "{fine} names a location");
        }
    }

    /// Review of car 2, R2-A (run 4533d570): the camelCase split read
    /// `PINs` as `pi`, `ns` — the break before an uppercase letter that is
    /// followed by a lowercase one is right for `APIKey` and wrong for an
    /// acronym's plural, whose lowercase letter is a lone final `s`. So
    /// `PINs`, `OTPs` and `JWTs` stopped being refused, a regression from
    /// ca5ccec3, which judged them before the split existed. A lone `s`
    /// that ends a word — the name's end, a separator, or the next
    /// capital — stays with the acronym.
    #[test]
    fn an_acronym_plural_is_one_word() {
        assert_eq!(name_words("PINs"), ["pins"]);
        assert_eq!(name_words("backupOTPs"), ["backup", "otps"]);
        assert_eq!(name_words("JWTs_file"), ["jwts", "file"]);
        assert_eq!(name_words("APIsKey"), ["apis", "key"]);
        assert_eq!(name_words("hostIPs"), ["host", "ips"]);
        for key in [
            "PINs",
            "OTPs",
            "JWTs",
            "recovery.PINs",
            "backupOTPs",
            "APIKeys",
        ] {
            assert!(secret_key(key), "{key} names a secret (R2-A)");
        }
        for fine in ["URLs", "IPs", "hostIPs", "ACLs"] {
            assert!(!secret_key(fine), "{fine} names a location (R2-A)");
        }
    }

    /// Round-3 R3-2: a PATTERN names what it reads too — the literal left
    /// of its capture (`BOSS_ADMIN_PASSWORD=`, `{name: DB_PASS, value:`) —
    /// and that name is judged the way a key's is, wherever the pattern
    /// rides: a fact line, a command's `with`, a road's `needs`.
    #[test]
    fn a_pattern_whose_literal_names_a_secret_is_refused() {
        let refused = [
            r"pattern = '^Environment=BOSS_ADMIN_PASSWORD=(\S+)$'",
            r#"pattern = '\{name: DB_PASS, value: "([^"]+)"\}'"#,
            r"pattern = '^\s+apiKey: (\S+)$'",
        ];
        for read in refused {
            let src = format!(
                "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nlabel = \"l\"\nfile = \"f.yaml\"\n{read}\n"
            );
            let problems = parse_sheet(&src).expect_err(read);
            assert!(
                problems
                    .iter()
                    .any(|p| p.contains("name says it holds a secret")),
                "{read}: {problems:?}"
            );
        }
        let with = r#"[[road]]
id = "r"
title = "t"
[[road.line]]
label = "l"
command = "x {a}"
with = { a = { file = "f.service", pattern = '^Environment=FORGE_TOKEN=(\S+)$' } }
"#;
        let problems = parse_sheet(with).expect_err("a command's pattern");
        assert!(
            problems
                .iter()
                .any(|p| p.contains("name says it holds a secret")),
            "{problems:?}"
        );
        let needs = r#"[[road]]
id = "r"
title = "t"
needs = { host = { file = "m.yaml", pattern = '\{name: BASIC_AUTH, value: "([^"]+)"\}' } }
[[road.line]]
prose = "p"
"#;
        let problems = parse_sheet(needs).expect_err("a road's needs pattern");
        assert!(
            problems
                .iter()
                .any(|p| p.contains("name says it holds a secret")),
            "{problems:?}"
        );
        // A location's name is not a secret's: every pattern the tree's
        // own sheet reads by passes (the_recovery_sheet_resolves_every_fact
        // renders it), and so do these.
        for fine in [
            r"^User=(\S+)$",
            r#"\{name: BOSS_AUTH_PROVIDER, value: ([^}\s]+)\}"#,
            r"\{secretName: ([a-z-]*authorized-keys),",
            r"^\s+port: (\d+)$",
        ] {
            assert_eq!(pattern_names_a_secret(fine), None, "{fine}");
        }
    }

    /// Round-3 R3-2: a Secret is a Secret however the manifest spells its
    /// kind — a list item, quoted, a trailing comment, flow style, JSON.
    #[test]
    fn a_secret_is_declared_in_every_spelling_of_its_kind() {
        for text in [
            "kind: Secret\n",
            "---\n- kind: Secret\n  metadata: {}\n",
            "kind: \"Secret\"\n",
            "kind: 'Secret'\n",
            "  kind: Secret   # the token\n",
            "{\"apiVersion\":\"v1\",\"kind\":\"Secret\",\"data\":{}}",
            "{\n  \"kind\": \"Secret\"\n}\n",
            "items:\n  - {kind: Secret, metadata: {name: x}}\n",
        ] {
            assert!(declares_a_secret(text), "{text:?}");
        }
        for text in [
            "kind: ServiceAccount\n",
            "kind: SecretProviderClass\n",
            "  secretName: op-token\n",
            "# the Secret named below is mounted\n",
            "{\"kind\":\"ConfigMap\"}",
        ] {
            assert!(!declares_a_secret(text), "{text:?}");
        }
    }

    /// Round-3 R3-1: the forge's admin kubeconfig is this road's whole
    /// point and no reader has seen it in place — the forge's observer
    /// reports its operator credentials unmeasured, because the directory
    /// is root's alone. The paper says so, as the bastion's road does for
    /// its copy, rather than implying a door that may not open.
    #[test]
    fn the_cluster_admin_road_says_its_credential_is_not_proven_in_place() {
        let r = tree_sheet();
        let admin = road(&r, "cluster-admin");
        assert!(
            admin.lines.iter().any(|l| matches!(
                l,
                LineOut::NotHeld(n) if n.contains("admin kubeconfig") && n.contains("unmeasured")
            )),
            "{:?}",
            admin.lines
        );
    }

    /// The footer says when the page was rendered in the reader's own time
    /// as well as UTC: the stack runs UTC and the one human reading the
    /// paper lives in America/Los_Angeles. Both 2026 changes, each side.
    #[test]
    fn the_render_time_is_printed_in_pacific_time_too() {
        let at = |s: &str| {
            chrono::DateTime::parse_from_rfc3339(s)
                .unwrap()
                .with_timezone(&chrono::Utc)
        };
        for (utc, pacific) in [
            ("2026-09-29T06:11:00Z", "2026-09-28 23:11 PDT"),
            ("2026-01-15T20:00:00Z", "2026-01-15 12:00 PST"),
            ("2026-03-08T09:59:00Z", "2026-03-08 01:59 PST"),
            ("2026-03-08T10:00:00Z", "2026-03-08 03:00 PDT"),
            ("2026-11-01T08:59:00Z", "2026-11-01 01:59 PDT"),
            ("2026-11-01T09:00:00Z", "2026-11-01 01:00 PST"),
            ("2027-03-14T10:00:00Z", "2027-03-14 03:00 PDT"),
        ] {
            assert_eq!(pacific_time(at(utc)).as_deref(), Some(pacific), "{utc}");
        }
        let stamp = rendered_at(at("2026-09-29T06:11:00Z"));
        assert_eq!(
            stamp,
            "2026-09-29 06:11 UTC (2026-09-28 23:11 PDT, America/Los_Angeles)"
        );
        assert!(tree_sheet().html("sha", &stamp).contains("23:11 PDT"));
    }

    /// Design 125d405d §6, car 2: the sheet printed to PDF by the boss-ci
    /// image's Chromium and READ BACK — every value `--facts` prints is
    /// in the PDF's text, the render time in the footer is too, and the
    /// digest recorded is the digest of the bytes handed over. Runs the
    /// real browser: the gate's image bakes it, and a PDF check against a
    /// fake printer would prove the fake. The control shows the check can
    /// fail — a sheet that says more than the printed page is named by
    /// where it says it.
    #[test]
    fn the_pdf_says_everything_the_facts_say() {
        let (sheet, at, printed, runs) = printed_tree_sheet();
        assert!(printed.bytes.starts_with(b"%PDF-"));
        assert_eq!(
            printed.sha256,
            hex::encode(Sha256::digest(&printed.bytes)),
            "the digest is of the bytes handed over"
        );
        // Judged again here, independently of the verb's own read-back.
        assert_eq!(
            sheet.judge_print(&runs, &sheet.footer(TREE, &at)),
            Vec::<String>::new()
        );
        let text = pdf::join(&runs);
        for words in [
            "23:11 PDT",
            TREE,
            NOT_HELD,
            NOT_EXERCISED,
            &sheet.hash()[..12],
        ] {
            assert!(text.contains(words), "{words:?} in:\n{text}");
        }
        assert!(
            !text.contains("file://"),
            "the browser's own header and footer are off: {text}"
        );

        // The control: a sheet saying one line more than the page is
        // named by where it says it.
        let mut more = sheet.clone();
        more.roads[0].lines.push(LineOut::Prose(
            "a line the printed page never carried".into(),
        ));
        let missing = more.missing_from(&text);
        let line = format!(
            "road 1 ({}) line {} prose",
            more.roads[0].id,
            more.roads[0].lines.len()
        );
        assert!(missing.contains(&line), "{missing:?}");
    }

    const TREE: &str = "0123456789abcdef";

    /// The tree's sheet printed once by the real browser, with the render
    /// time it was printed at, and read back into positioned runs.
    fn printed_tree_sheet() -> (Rendered, String, Printed, Vec<pdf::Run>) {
        let sheet = tree_sheet();
        let at = rendered_at(
            chrono::DateTime::parse_from_rfc3339("2026-09-29T06:11:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let printed = sheet
            .print_pdf(TREE, &at, None)
            .unwrap_or_else(|e| panic!("{e}"));
        let runs = pdf::runs(&printed.bytes).unwrap_or_else(|e| panic!("{e}"));
        (sheet, at, printed, runs)
    }

    /// Review of car 2, F1: the footer prints in a page margin of its own
    /// on EVERY page, and nothing else enters that band — measured on the
    /// real print, page by page, from the positions the reader walks. The
    /// control plants one body run in page 2's band and is refused by
    /// page. (The fixed-position footer this replaced printed over road
    /// 1's and road 7's continuations and passed the text-only check.)
    #[test]
    fn the_footer_prints_in_its_own_band_on_every_page() {
        let (sheet, at, _, runs) = printed_tree_sheet();
        let pages = runs.iter().map(|r| r.page + 1).max().unwrap_or(0);
        assert!(pages >= 2, "the sheet runs to several pages: {pages}");
        let footer = squash(&sheet.footer(TREE, &at));
        for page in 0..pages {
            let band: Vec<pdf::Run> = runs
                .iter()
                .filter(|r| r.page == page && r.y < FOOTER_BAND_PT)
                .cloned()
                .collect();
            assert_eq!(squash(&pdf::join(&band)), footer, "page {}", page + 1);
        }
        let mut over = runs.clone();
        over.push(pdf::Run {
            page: 1,
            y: FOOTER_BAND_PT - 20.0,
            text: "to.json".into(),
        });
        let problems = sheet.judge_print(&over, &sheet.footer(TREE, &at));
        assert!(
            problems.iter().any(|p| p.starts_with("page 2:")),
            "{problems:?}"
        );
    }

    /// Review of car 2, F3: a PDF printed with another render time (or
    /// another tree, or another version) is not this print, and is
    /// refused on every page — deleting the footer check fails here.
    #[test]
    fn a_pdf_printed_with_another_stamp_is_refused() {
        let (sheet, at, _, runs) = printed_tree_sheet();
        let pages = runs.iter().map(|r| r.page + 1).max().unwrap_or(0);
        for footer in [
            sheet.footer(TREE, &at.replace("06:11", "06:12")),
            sheet.footer("fedcba9876543210", &at),
            sheet
                .footer(TREE, &at)
                .replace(&sheet.hash()[..12], "000000000000"),
        ] {
            let problems = sheet.judge_print(&runs, &footer);
            assert_eq!(
                problems
                    .iter()
                    .filter(|p| p.contains("bottom band"))
                    .count(),
                pages,
                "{footer}: {problems:?}"
            );
        }
    }

    /// Review of car 2, F2: a fact is found as `Label: value` in its own
    /// road's stretch — so a short value that also appears elsewhere
    /// ("22", "dev") does not pass for a line that never printed.
    #[test]
    fn a_short_value_is_found_only_as_its_own_line() {
        let (sheet, _, _, runs) = printed_tree_sheet();
        let text = pdf::join(&runs);
        assert!(text.contains("22") && text.contains("dev"));
        let mut more = sheet.clone();
        for (label, value) in [("Port", "22"), ("Session", "dev")] {
            more.roads[0].lines.push(LineOut::Fact {
                label: label.into(),
                source: "f#k".into(),
                value: Value::Text(value.into()),
            });
        }
        let missing = more.missing_from(&text);
        let n = more.roads[0].lines.len();
        for (i, label) in [(n - 1, "Port"), (n, "Session")] {
            let at = format!("road 1 ({}) line {i} fact ({label})", more.roads[0].id);
            assert!(missing.contains(&at), "{at}: {missing:?}");
        }
        // And a line moved to another road's stretch is not found in its
        // own: the last road's title, claimed by road 1.
        let mut moved = sheet.clone();
        let last = moved
            .roads
            .last()
            .map(|r| r.title.clone())
            .unwrap_or_default();
        moved.roads[0].lines.push(LineOut::Prose(last));
        assert!(!moved.missing_from(&text).is_empty());
    }

    /// Round-2 R2-6: a command fills in ONE pass, so a value holding
    /// `{b}` prints as written instead of being filled again; and only
    /// dev-door.json templates its own strings.
    #[test]
    fn a_fill_is_one_pass_and_only_the_door_file_templates_itself() {
        let values = BTreeMap::from([
            ("a".to_string(), "{b}".to_string()),
            ("b".to_string(), "second".to_string()),
        ]);
        assert_eq!(fill_once("x {a} {b}", &values).unwrap(), "x {b} second");

        let src = "[[road]]\nid = \"r\"\ntitle = \"t\"\n[[road.line]]\nlabel = \"l\"\ncommand = \"x {a} {b}\"\nwith = { a = { file = \"f.toml\", key = \"a\" }, b = { file = \"f.toml\", key = \"b\" } }\n";
        let dir = scratch_dir("recovery-one-pass");
        write_file(&dir.join("f.toml"), "a = \"{b}\"\nb = \"second\"\n");
        let r = resolve(&dir, &parse_sheet(src).unwrap()).unwrap();
        assert_eq!(fact(&r.roads[0], "l"), &Value::Text("x {b} second".into()));

        let other = one_fact(
            "recovery-no-fill",
            "d.json",
            "key = \"say\"",
            "{\"host\": \"h\", \"say\": \"at {host}\"}",
        )
        .unwrap();
        assert_eq!(fact(&other.roads[0], "l"), &Value::Text("at {host}".into()));
    }

    #[test]
    fn the_page_escapes_what_it_prints() {
        let r = Rendered {
            roads: vec![RoadOut {
                id: "r".into(),
                title: "a <b>".into(),
                proven_by: None,
                lines: vec![LineOut::Fact {
                    label: "l".into(),
                    source: "f#k".into(),
                    value: Value::Text("x & \"y\"".into()),
                }],
                needs: vec![],
                files: BTreeSet::new(),
            }],
        };
        let html = r.html("sha", "now");
        assert!(
            html.contains("a &lt;b&gt;") && html.contains("x &amp; &quot;y&quot;"),
            "{html}"
        );
        assert!(html.contains(NOT_EXERCISED));
    }
}
