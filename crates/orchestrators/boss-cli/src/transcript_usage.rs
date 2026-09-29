//! What a dispatched run CONSUMED, read from the harness's own
//! transcript of it (backlog e6b2066f).
//!
//! THE NUMBER THIS REPLACES. A builder's handback carried the harness's
//! `subagent_tokens`, and `boss dispatch --report --tokens` recorded it
//! as the run's `total_tokens` — priced at a blend, read by both budget
//! desks. It is the size of the run's FINAL context window: it matched
//! the last turn's four counts within 1% on 62 of 68 runs, while the
//! tokens the run was billed for, summed over every turn, were a median
//! 48x larger, 96.8% of them cache reads. Real spend was a median 4.9x
//! the recorded figure.
//!
//! THE NUMBER THAT WAS ALWAYS THERE. Claude Code writes every subagent's
//! turns to `<projects>/<project>/<session>/subagents/agent-<id>.jsonl`,
//! and every assistant line carries the turn's `message.usage`:
//! `input_tokens` (uncached), `cache_creation_input_tokens`,
//! `cache_read_input_tokens` and `output_tokens`. [`sum_usage`] sums
//! those four over the run — reading the record rather than retyping a
//! figure from a handback, the "receipt copied, not retyped" rule.
//!
//! ONE TURN IS SEVERAL LINES. The harness writes a line per content
//! block (thinking, text, each tool call), each carrying the SAME
//! `message.id` and the same prompt counts, with `output_tokens` growing
//! as the turn streams. A naive sum counts a three-block turn three
//! times, so turns are keyed on `message.id` (else `requestId`) and each
//! count is the turn's largest — measured on this car's own transcript,
//! where one turn's lines read output 5, then 155.
//!
//! WHICH TRANSCRIPT. [`find_transcripts`] looks for the one whose text
//! holds `agent-run <run id>` — the phrase every run section prints
//! (`dispatch::run_section`), whether the prompt carried it or the
//! builder read it from a prompt file — among subagent transcripts
//! written since the run opened. Exactly one is the run's; none, or
//! more than one, is said out loud and the report falls back to the
//! count it was given, because guessing between two transcripts would
//! record one run's spend against another.
//!
//! WHICH TURNS. One agent may run several dispatched runs and write
//! them all to one transcript, so a report counts only its run's
//! [`Slice`] of the file — after the run's start and the last report
//! already metered from the same file, up to its own instant — and
//! records the bounds it read (backlog 4f74727b). A transcript that
//! carries each run's [`marker`] is cut on those first, so a batch
//! reported all at once still splits by run (backlog 11a0998a).
//!
//! WHICH MODEL. The same transcript says which model every turn was
//! billed as, so the record names that model rather than the one the
//! step's agent block declared ([`RunModels`], backlog 6bb85880).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde_json::Value;

/// A run's four billed counts, summed over its turns, plus the two
/// readings that say what the old figure was and where the floor is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Usage {
    pub input: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
    /// Of `cache_write`, the tokens written to the 1-HOUR cache, which
    /// costs more than the 5-minute rate the card prices every write
    /// at — so a nonzero figure here says the price is a floor.
    pub cache_write_1h: u64,
    /// Distinct turns summed.
    pub turns: u64,
    /// The LAST turn's four-way sum: the final context size, which is
    /// what the harness's `subagent_tokens` reports. Kept beside the
    /// sum so the two can be compared on the record.
    pub final_context: u64,
}

impl Usage {
    /// Everything the run processed.
    pub(crate) fn total(&self) -> u64 {
        self.input
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
            .saturating_add(self.output)
    }
}

/// One turn's counts, as the largest seen on any of its lines.
#[derive(Debug, Clone, Copy, Default)]
struct Turn {
    input: u64,
    cache_write: u64,
    cache_read: u64,
    output: u64,
    cache_write_1h: u64,
}

impl Turn {
    fn widen(self, other: Turn) -> Turn {
        Turn {
            input: self.input.max(other.input),
            cache_write: self.cache_write.max(other.cache_write),
            cache_read: self.cache_read.max(other.cache_read),
            output: self.output.max(other.output),
            cache_write_1h: self.cache_write_1h.max(other.cache_write_1h),
        }
    }

    fn context(&self) -> u64 {
        self.input
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
            .saturating_add(self.output)
    }
}

/// Sum a transcript's per-turn usage. `None` when no line carries any —
/// no count is not a count of zero. Lines that are not JSON, or not an
/// assistant turn, are skipped: the transcript holds user turns, tool
/// results and summaries beside the turns that were billed.
pub(crate) fn sum_usage(jsonl: &str) -> Option<Usage> {
    let mut order: Vec<String> = Vec::new();
    let mut turns: std::collections::HashMap<String, Turn> = std::collections::HashMap::new();
    for (n, line) in jsonl.lines().enumerate() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(u) = v.pointer("/message/usage").filter(|u| u.is_object()) else {
            continue;
        };
        let count = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        let turn = Turn {
            input: count("input_tokens"),
            cache_write: count("cache_creation_input_tokens"),
            cache_read: count("cache_read_input_tokens"),
            output: count("output_tokens"),
            cache_write_1h: u
                .pointer("/cache_creation/ephemeral_1h_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        };
        let key = turn_key(&v)
            .map(str::to_string)
            .unwrap_or_else(|| format!("line-{n}"));
        match turns.get(&key) {
            Some(seen) => {
                let widened = seen.widen(turn);
                turns.insert(key, widened);
            }
            None => {
                order.push(key.clone());
                turns.insert(key, turn);
            }
        }
    }
    let last = order.last().and_then(|k| turns.get(k)).copied()?;
    Some(order.iter().filter_map(|k| turns.get(k)).fold(
        Usage {
            final_context: last.context(),
            ..Usage::default()
        },
        |acc, t| Usage {
            input: acc.input.saturating_add(t.input),
            cache_write: acc.cache_write.saturating_add(t.cache_write),
            cache_read: acc.cache_read.saturating_add(t.cache_read),
            output: acc.output.saturating_add(t.output),
            cache_write_1h: acc.cache_write_1h.saturating_add(t.cache_write_1h),
            turns: acc.turns + 1,
            final_context: acc.final_context,
        },
    ))
}

/// Where the harness keeps its projects: `$CLAUDE_CONFIG_DIR/projects`
/// when the session set one, else `$HOME/.claude/projects`.
pub(crate) fn projects_root() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(|d| PathBuf::from(d).join("projects"))
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".claude").join("projects"))
        })
}

/// The phrase a run's own transcript holds and no other run's does.
pub(crate) fn needle(run_id: &str) -> String {
    format!("agent-run {run_id}")
}

/// Every subagent transcript under `root` (`<project>/<session>/
/// subagents/agent-*.jsonl`) written at or after `since` whose text
/// holds [`needle`]. The caller decides what none or several mean.
pub(crate) fn find_transcripts(root: &Path, run_id: &str, since: SystemTime) -> Vec<PathBuf> {
    let needle = needle(run_id);
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default()
    };
    let mut found: Vec<PathBuf> = dirs(root)
        .into_iter()
        .flat_map(|project| dirs(&project))
        .flat_map(|session| dirs(&session.join("subagents")))
        .filter(|f| {
            f.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("agent-") && n.ends_with(".jsonl"))
        })
        .filter(|f| {
            std::fs::metadata(f)
                .and_then(|m| m.modified())
                .is_ok_and(|t| t >= since)
        })
        .filter(|f| {
            std::fs::read_to_string(f)
                .map(|text| text.contains(&needle))
                .unwrap_or(false)
        })
        .collect();
    found.sort();
    found
}

/// Which model a run ran on, as its transcript says (backlog 6bb85880).
///
/// THE WORD THIS REPLACES. The record took the model from the run
/// packet — the Workflow agent block's `model`, `opus-5[1m]` on all 20
/// blocks — and priced the run at that row. Measured 2026-09-24: the
/// newest subagent transcripts say `claude-opus-5-5` on every billed
/// turn, because the agent definitions say `model: opus`, an alias the
/// harness resolves to the newest Opus. The block was a declaration;
/// the transcript is the receipt, and the meter already reads it.
///
/// Two readings, both the harness's own: every billed turn's
/// `message.model` (the API id the turn was billed as), and the model
/// attachment's `identity.modelId` (what the session was launched as,
/// which carries the `[1m]` context suffix the card spells). The turns
/// are the authority; the identity is believed only when it names the
/// same model, and then only for its spelling.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RunModels {
    /// Every distinct model id a billed turn names, first-seen order.
    /// `<synthetic>` is the harness speaking — a zero-usage line no model
    /// produced — and is not a model.
    pub billed: Vec<String>,
    /// The harness's model attachment, when the transcript has one.
    pub identity: Option<String>,
}

impl RunModels {
    /// The model as `agent_rate_card` spells it, or `None` when the
    /// transcript names none. Several billed models are named together
    /// (`opus-5-5+haiku-4-5`): the four counts are summed across them
    /// and cannot be priced at one row's rates, so no row names the
    /// pair and the run reads as unpriced — rather than being priced,
    /// wholly, at whichever model came first.
    pub(crate) fn recorded(&self) -> Option<String> {
        // Spelled before they are counted, so a dated id and its undated
        // twin are one model, not a pair no row names (backlog 8e1a2a6f).
        let billed = self.billed.iter().map(|m| card_spelling(m)).fold(
            Vec::<String>::new(),
            |mut acc, m| {
                if !acc.contains(&m) {
                    acc.push(m);
                }
                acc
            },
        );
        match billed.as_slice() {
            [] => self.identity.as_deref().map(card_spelling),
            [one] => Some(
                self.identity
                    .as_deref()
                    .map(card_spelling)
                    .filter(|i| i.split_once('[').map_or(i.as_str(), |(base, _)| base) == one)
                    .unwrap_or_else(|| one.clone()),
            ),
            many => Some(many.join("+")),
        }
    }
}

/// An API model id as the rate card spells it: without the `claude-`
/// prefix (20260910030644 keys the card on `opus-5`, not `claude-opus-5`)
/// and without a dated snapshot suffix, keeping any `[1m]` context
/// suffix.
///
/// THE DATE (backlog 8e1a2a6f). Haiku turns are billed as
/// `claude-haiku-4-5-20251001`, and this function used to keep the date,
/// so every Haiku run was recorded as a model no row names and read as
/// unpriced. A snapshot is priced as its model — the pricing page lists
/// models, never snapshots — so the date is dropped here rather than a
/// row written per snapshot. Matching against the card stays EXACT
/// (20260910030644): this spells the id, it does not look for a
/// neighbour. Only a trailing all-digit segment of eight is a date; a
/// version number is one or two digits.
pub(crate) fn card_spelling(api_id: &str) -> String {
    let bare = api_id.strip_prefix("claude-").unwrap_or(api_id);
    let (base, context) = bare
        .find('[')
        .map_or((bare, ""), |at| (&bare[..at], &bare[at..]));
    let undated = base
        .rsplit_once('-')
        .filter(|(_, tail)| tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()))
        .map_or(base, |(model, _)| model);
    format!("{undated}{context}")
}

/// The models a transcript names, per [`RunModels`].
pub(crate) fn read_models(jsonl: &str) -> RunModels {
    jsonl
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .fold(RunModels::default(), |mut acc, v| {
            let billed = (v.get("type").and_then(Value::as_str) == Some("assistant"))
                .then(|| v.pointer("/message/model").and_then(Value::as_str))
                .flatten()
                .filter(|m| *m != "<synthetic>" && !m.is_empty());
            if let Some(m) = billed
                && !acc.billed.iter().any(|b| b == m)
            {
                acc.billed.push(m.to_string());
            }
            if acc.identity.is_none()
                && v.pointer("/attachment/type").and_then(Value::as_str) == Some("model")
            {
                acc.identity = v
                    .pointer("/attachment/identity/modelId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            acc
        })
}

/// The part of a transcript one report may count (backlog 4f74727b).
///
/// THE RUNNING TOTAL THIS REPLACES. The meter summed a transcript from
/// its first line, and `since` chose which FILES to search, never which
/// turns to count. One agent that runs several dispatched runs writes
/// ONE transcript, so each later report recorded everything before it
/// again: the triage batch of 2026-09-28 (four runs, one agent,
/// `agent-abb03ad066e8e7222.jsonl`) recorded $0.93, $1.14, $1.49 and
/// $1.66 — the file's running total at each report — and the table
/// summed $5.21 for about $1.66 of work. Batching small runs into one
/// spawn is deliberate, so the meter supports it rather than the
/// batching stopping.
///
/// A run's turns are the ones after the LATER of two instants — the
/// run's own start, and the last report already metered from the same
/// file (every run in that batch was dispatched before the agent began,
/// so their starts separated nothing; the reports did) — and at or
/// before the report's own instant. Both bounds ride the record
/// ([`Sliced`]), so the figure can be re-derived from the file.
///
/// MARKERS FIRST (backlog 11a0998a). Report instants separate runs only
/// when each is reported before the next begins. When the PARENT
/// reports a whole batch after one handback, every report is after the
/// file's last line: the first took the whole transcript and the rest a
/// metered zero — the total right, the split wrong. So every run's
/// prompt carries a [`marker`], the transcript records the instant it
/// ARRIVED (the prompt, or the tool result that read it), and
/// [`Slice::marked`] cuts on those before it cuts on reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Slice {
    /// Turns at or before this instant are not this run's.
    pub after: Option<DateTime<Utc>>,
    /// What set `after`: `run start`, `report of run <id>`, or
    /// `marker of run <id>`.
    pub after_by: Option<String>,
    /// Turns after this instant are not this run's.
    pub until: Option<DateTime<Utc>>,
    /// What set `until`: `this report`, or `marker of run <id>` — the
    /// next run's marker.
    pub until_by: Option<String>,
    /// The run's own start and every earlier report metered from the
    /// same file ([`earlier_reports`]) — the bounds `after` was the
    /// latest of, kept because [`Slice::marked`] must weigh them one by
    /// one: a report of a run marked elsewhere in the file bounds
    /// nothing before that run's marker.
    started: Option<DateTime<Utc>>,
    reports: Vec<(DateTime<Utc>, String)>,
}

/// The latest of a run's start and the given reports, with what set it.
fn latest<'a>(
    started: Option<DateTime<Utc>>,
    reports: impl Iterator<Item = &'a (DateTime<Utc>, String)>,
) -> (Option<DateTime<Utc>>, Option<String>) {
    let start = started.map(|t| (t, "run start".to_string()));
    let report = reports
        .max_by_key(|(t, _)| *t)
        .map(|(t, run)| (*t, format!("report of run {run}")));
    match (start, report) {
        (Some(s), Some(r)) if r.0 > s.0 => (Some(r.0), Some(r.1)),
        (Some(s), _) => (Some(s.0), Some(s.1)),
        (None, Some(r)) => (Some(r.0), Some(r.1)),
        (None, None) => (None, None),
    }
}

impl Slice {
    /// The slice for a run that started at `started`, reported at
    /// `until`, with `earlier` the reports already metered from its
    /// transcript ([`earlier_reports`]).
    pub(crate) fn bounded(
        started: Option<DateTime<Utc>>,
        earlier: Vec<(DateTime<Utc>, String)>,
        until: DateTime<Utc>,
    ) -> Slice {
        let (after, after_by) = latest(started, earlier.iter());
        Slice {
            after,
            after_by,
            until: Some(until),
            until_by: Some("this report".to_string()),
            started,
            reports: earlier,
        }
    }

    /// This slice, cut on the transcript's markers (backlog 11a0998a).
    /// No marker in the file, and it is the slice it was.
    ///
    /// The markers split the file into segments — each from one marker
    /// to the next — and a run's segment is the one its own marker
    /// opens, up to its report. The first run's also holds the lines
    /// before any marker (the batch's preamble), bounded by its start
    /// and by the reports of runs that carry no marker, which are the
    /// only other runs that could have counted them.
    ///
    /// Markers that arrived TOGETHER (a prompt pasting several runs'
    /// sections at once) separate nothing between those runs, so they
    /// share their segment by report instants, as before. A run with NO
    /// marker in a marked file — dispatched before this car, reported
    /// after it — keeps its report bounds and stops at the next marker.
    /// Either way no turn is counted twice: a segment is one run's, or
    /// is split among its sharers by the report rule that already
    /// conserved.
    ///
    /// The turn that reads the next run's prompt is before that
    /// marker arrives, so it is billed to the run it ends.
    pub(crate) fn marked(self, marks: &[Mark], run_id: &str) -> Slice {
        if marks.is_empty() {
            return self;
        }
        let own = marks.iter().find(|m| m.run == run_id).map(|m| m.at);
        let shared = own.is_some_and(|t| marks.iter().any(|m| m.run != run_id && m.at == t));
        let first = own.is_some_and(|t| marks.iter().all(|m| m.at >= t));
        // The runs whose reports can bound this run's segment: runs
        // with no marker, and runs sharing this run's marker.
        let rival = |run: &str| {
            marks
                .iter()
                .find(|m| m.run == run)
                .is_none_or(|m| Some(m.at) == own)
        };
        let by_marker = |t: DateTime<Utc>| (Some(t), Some(format!("marker of run {run_id}")));
        let (after, after_by) = match own {
            None => (self.after, self.after_by.clone()),
            Some(t) => {
                let (b, b_by) = latest(
                    self.started,
                    self.reports.iter().filter(|(_, run)| rival(run)),
                );
                match (shared, first) {
                    (true, true) => (b, b_by),
                    (true, false) if b.is_some_and(|b| b > t) => (b, b_by),
                    (false, true) if b.is_none_or(|b| b <= t) => (b, b_by),
                    _ => by_marker(t),
                }
            }
        };
        let from = own.or(after);
        let next = marks
            .iter()
            .filter(|m| from.is_none_or(|f| m.at > f))
            .min_by_key(|m| m.at);
        let (until, until_by) = match next {
            Some(m) if self.until.is_none_or(|u| m.at < u) => {
                (Some(m.at), Some(format!("marker of run {}", m.run)))
            }
            _ => (self.until, self.until_by.clone()),
        };
        Slice {
            after,
            after_by,
            until,
            until_by,
            ..self
        }
    }
}

/// The line every run's prompt carries (`dispatch::run_section`), which
/// [`markers`] reads back: the instant it arrived is where the run
/// begins. It holds [`needle`], so a marked transcript is found too.
pub(crate) fn marker(run_id: &str) -> String {
    format!("{MARKER_HEAD}{run_id}{MARKER_TAIL}")
}
const MARKER_HEAD: &str = "run-marker: agent-run ";
const MARKER_TAIL: &str = " begins here";

/// A run's marker as the transcript recorded it: when it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mark {
    pub at: DateTime<Utc>,
    pub run: String,
}

/// Each run's FIRST marker, in file order. Read only off `user` lines —
/// the prompt, and tool results, which is what reading a prompt file
/// or running a claim leaves — because an assistant line is the agent
/// speaking, and it may quote a marker it has not reached. A line with
/// no `timestamp` takes the last one above it, as in [`cut`].
pub(crate) fn markers(jsonl: &str) -> Vec<Mark> {
    let mut last_seen: Option<DateTime<Utc>> = None;
    let mut found: Vec<Mark> = Vec::new();
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(t) = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
        {
            last_seen = Some(t);
        }
        if v.get("type").and_then(Value::as_str) != Some("user") || !line.contains(MARKER_HEAD) {
            continue;
        }
        let Some(at) = last_seen else {
            continue;
        };
        for run in marked_runs(line) {
            if !found.iter().any(|m| m.run == run) {
                found.push(Mark { at, run });
            }
        }
    }
    found
}

/// The run ids `text` marks: an id-shaped word between the marker's
/// head and tail, so a placeholder or a phrase without the tail is not
/// a marker.
fn marked_runs(text: &str) -> Vec<String> {
    text.match_indices(MARKER_HEAD)
        .filter_map(|(at, _)| {
            let rest = &text[at + MARKER_HEAD.len()..];
            let id: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            (!id.is_empty() && rest[id.len()..].starts_with(MARKER_TAIL)).then_some(id)
        })
        .collect()
}

/// Every report already metered from `transcript`, among `rows`
/// (`GET /api/agent-runs`), oldest first: the `finished_at` — a
/// report's own instant — of each other run whose
/// `detail.metered.transcript` names the same file, before `until`.
/// Files are matched by NAME: the harness names a subagent transcript
/// `agent-<random id>.jsonl`, and an operator's `--transcript` may
/// spell the directory differently.
pub(crate) fn earlier_reports(
    rows: &[Value],
    transcript: &Path,
    run_id: &str,
    until: DateTime<Utc>,
) -> Vec<(DateTime<Utc>, String)> {
    let Some(name) = transcript.file_name() else {
        return Vec::new();
    };
    let mut found: Vec<(DateTime<Utc>, String)> = rows
        .iter()
        .filter(|r| r.get("run_id").and_then(Value::as_str) != Some(run_id))
        .filter(|r| {
            r.pointer("/detail/metered/transcript")
                .and_then(Value::as_str)
                .is_some_and(|t| Path::new(t).file_name() == Some(name))
        })
        .filter_map(|r| {
            let at = r
                .get("finished_at")
                .and_then(Value::as_str)?
                .parse::<DateTime<Utc>>()
                .ok()?;
            let run = r.get("run_id").and_then(Value::as_str)?.to_string();
            Some((at, run))
        })
        .filter(|(at, _)| *at < until)
        .collect();
    found.sort();
    found
}

/// The key one turn's lines share: `message.id`, else `requestId`.
fn turn_key(v: &Value) -> Option<&str> {
    v.pointer("/message/id")
        .or_else(|| v.get("requestId"))
        .and_then(Value::as_str)
}

/// The lines of `jsonl` inside `slice`, with the first and last line
/// numbers (1-based) kept. A line without a `timestamp` takes the last
/// one written above it — the file is appended in time order. A turn is
/// judged by its FIRST line, so a turn streamed across a bound is one
/// run's whole and none of the other's. The model attachment is the
/// session's, not a turn's, and belongs to every slice of it.
pub(crate) fn cut(jsonl: &str, slice: &Slice) -> (String, Option<usize>, Option<usize>) {
    let mut last_seen: Option<DateTime<Utc>> = None;
    let mut turn_at: std::collections::HashMap<String, Option<DateTime<Utc>>> =
        std::collections::HashMap::new();
    let mut kept: Vec<&str> = Vec::new();
    let (mut first, mut last) = (None, None);
    for (n, line) in jsonl.lines().enumerate() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.pointer("/attachment/type").and_then(Value::as_str) == Some("model") {
            kept.push(line);
            continue;
        }
        if let Some(t) = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
        {
            last_seen = Some(t);
        }
        let at = match (v.get("type").and_then(Value::as_str), turn_key(&v)) {
            (Some("assistant"), Some(k)) => *turn_at.entry(k.to_string()).or_insert(last_seen),
            _ => last_seen,
        };
        let inside = slice.after.is_none_or(|a| at.is_some_and(|t| t > a))
            && slice.until.is_none_or(|u| at.is_none_or(|t| t <= u));
        if inside {
            kept.push(line);
            first = first.or(Some(n + 1));
            last = Some(n + 1);
        }
    }
    let mut text = kept.join("\n");
    text.push('\n');
    (text, first, last)
}

/// Where in the transcript a metered figure came from, so it can be
/// re-derived by reading the same lines (backlog 4f74727b).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Sliced {
    pub after: Option<DateTime<Utc>>,
    pub after_by: Option<String>,
    pub until: Option<DateTime<Utc>>,
    /// What set `until`: the report, or the next run's marker.
    pub until_by: Option<String>,
    /// The first and last transcript line (1-based) the slice held.
    pub first_line: Option<usize>,
    pub last_line: Option<usize>,
    /// Why a metered count is zero, when it is: the transcript was read
    /// and holds no turn of this run. A zero with its reason, never a
    /// missing count, because the reading was made.
    pub zero_reason: Option<String>,
}

/// What the report read, and from where.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Metered {
    pub path: PathBuf,
    pub usage: Usage,
    /// The model the transcript says ran (backlog 6bb85880).
    pub models: RunModels,
    /// Where its tool time and context went (backlog 2f23f4c6).
    pub profile: boss_jobs::agent_runs::WorkProfile,
    /// Which lines of `path` were counted (backlog 4f74727b).
    pub slice: Sliced,
}

/// The run's transcript: `explicit` when the operator named one, else
/// the one transcript [`find_transcripts`] finds. `Err` is a sentence
/// saying why none was chosen — printed, and the report goes on with
/// the count it was given.
pub(crate) fn locate(
    explicit: Option<&Path>,
    root: Option<&Path>,
    run_id: &str,
    since: SystemTime,
) -> Result<PathBuf, String> {
    Ok(match explicit {
        Some(p) => p.to_path_buf(),
        None => {
            let root = root.ok_or(
                "no transcript directory (neither CLAUDE_CONFIG_DIR nor HOME is set) — pass \
                 --transcript <path>",
            )?;
            let mut found = find_transcripts(root, run_id, since);
            match found.len() {
                1 => found.remove(0),
                0 => {
                    return Err(format!(
                        "no subagent transcript under {} names {:?} — pass --transcript <path> \
                         to meter this run",
                        root.display(),
                        needle(run_id)
                    ));
                }
                _ => {
                    return Err(format!(
                        "{} transcripts name {:?} ({}) — refusing to guess which is this run's; \
                         pass --transcript <path>",
                        found.len(),
                        needle(run_id),
                        found
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }
    })
}

/// The run's metered usage: the turns of `path` inside `slice`.
///
/// A ZERO IS A READING. When the file holds billed turns but none of
/// this run's — the slice is empty, or a transcript named outright
/// never names the run — the count is a metered zero with its reason
/// on the record, not a missing count and not the file's whole spend.
/// `Err` is kept for a file that could not be read or holds no billed
/// turn at all, where nothing was measured.
pub(crate) fn meter(path: &Path, run_id: &str, slice: &Slice) -> Result<Metered, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read transcript {}: {e}", path.display()))?;
    if sum_usage(&text).is_none() {
        return Err(format!("transcript {} holds no turn usage", path.display()));
    }
    let named = text.contains(&needle(run_id));
    // Markers first, report instants second (backlog 11a0998a).
    let slice = &slice.clone().marked(&markers(&text), run_id);
    let (sliced, first_line, last_line) = match named {
        true => cut(&text, slice),
        false => (String::new(), None, None),
    };
    let (usage, zero_reason) = match (named, sum_usage(&sliced)) {
        (false, _) => (
            Usage::default(),
            Some(format!(
                "transcript never names {:?}, so it holds no turn of this run",
                needle(run_id)
            )),
        ),
        (true, Some(u)) => (u, None),
        (true, None) => (
            Usage::default(),
            Some(format!(
                "no billed turn after {} and at or before {} — every turn in the file is \
                 outside this run's slice",
                slice
                    .after
                    .map_or("the file's start".to_string(), |t| t.to_rfc3339()),
                slice
                    .until
                    .map_or("the file's end".to_string(), |t| t.to_rfc3339()),
            )),
        ),
    };
    Ok(Metered {
        path: path.to_path_buf(),
        usage,
        models: read_models(&sliced),
        profile: crate::transcript_profile::work_profile(&sliced),
        slice: Sliced {
            after: slice.after,
            after_by: slice.after_by.clone(),
            until: slice.until,
            until_by: slice.until_by.clone(),
            first_line,
            last_line,
            zero_reason,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Two lines of one turn (the thinking block, then the tool call,
    /// output growing 5 -> 155) and one line of the next — the shape
    /// measured on this car's own transcript, 2026-09-24.
    const TRANSCRIPT: &str = concat!(
        r#"{"type":"user","message":{"role":"user","content":"Your run is agent-run r-1"}}"#,
        "\n",
        r#"{"type":"assistant","requestId":"req_a","message":{"id":"msg_a","usage":{"input_tokens":2,"cache_creation_input_tokens":38126,"cache_read_input_tokens":14756,"output_tokens":5,"cache_creation":{"ephemeral_5m_input_tokens":38126,"ephemeral_1h_input_tokens":0}}}}"#,
        "\n",
        r#"{"type":"assistant","requestId":"req_a","message":{"id":"msg_a","usage":{"input_tokens":2,"cache_creation_input_tokens":38126,"cache_read_input_tokens":14756,"output_tokens":155}}}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result"}]}}"#,
        "\n",
        "not json at all\n",
        r#"{"type":"assistant","requestId":"req_b","message":{"id":"msg_b","usage":{"input_tokens":2,"cache_creation_input_tokens":6024,"cache_read_input_tokens":52882,"output_tokens":311,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":6024}}}}"#,
        "\n",
    );

    #[test]
    fn a_turn_written_as_several_lines_is_counted_once_at_its_largest() {
        let u = sum_usage(TRANSCRIPT).expect("two turns carry usage");
        assert_eq!(u.turns, 2);
        assert_eq!(u.input, 4);
        assert_eq!(u.cache_write, 38_126 + 6_024);
        assert_eq!(u.cache_read, 14_756 + 52_882);
        assert_eq!(
            u.output,
            155 + 311,
            "the streamed 5 is superseded, not added"
        );
        assert_eq!(u.cache_write_1h, 6_024);
        assert_eq!(
            u.final_context,
            2 + 6_024 + 52_882 + 311,
            "the last turn alone is the final context — the old figure"
        );
        assert_eq!(u.total(), 4 + 44_150 + 67_638 + 466);
    }

    #[test]
    fn a_transcript_with_no_turn_usage_is_no_count_not_zero() {
        assert_eq!(sum_usage(""), None);
        assert_eq!(
            sum_usage(r#"{"type":"user","message":{"content":"hi"}}"#),
            None
        );
    }

    /// The shape measured on this car's own transcript, 2026-09-25: the
    /// harness's model attachment names `claude-opus-5-5[1m]`, every
    /// billed turn names `claude-opus-5-5`, and a `<synthetic>` line —
    /// the harness speaking, zero usage — rides beside them.
    const OPUS_5_5: &str = concat!(
        r#"{"type":"attachment","attachment":{"type":"model","identity":{"modelId":"claude-opus-5-5[1m]","marketingName":"Opus 5.5 (1M context)"}}}"#,
        "\n",
        r#"{"type":"assistant","message":{"id":"msg_a","model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":900,"output_tokens":7}}}"#,
        "\n",
        r#"{"type":"assistant","message":{"id":"msg_s","model":"<synthetic>","usage":{"input_tokens":0,"output_tokens":0}}}"#,
        "\n",
        r#"{"type":"assistant","message":{"id":"msg_b","model":"claude-opus-5-5","usage":{"input_tokens":1,"cache_creation_input_tokens":10,"cache_read_input_tokens":1000,"output_tokens":9}}}"#,
        "\n",
    );

    /// WHICH MODEL RAN is read from the transcript it was billed in
    /// (backlog 6bb85880): the record said `opus-5[1m]` — the Workflow
    /// block's word — for runs whose every turn said `claude-opus-5-5`.
    #[test]
    fn the_model_is_read_from_the_transcript_and_spelled_as_the_card_spells_it() {
        let m = read_models(OPUS_5_5);
        assert_eq!(m.billed, vec!["claude-opus-5-5".to_string()]);
        assert_eq!(m.identity.as_deref(), Some("claude-opus-5-5[1m]"));
        assert_eq!(
            m.recorded().as_deref(),
            Some("opus-5-5[1m]"),
            "the identity names the billed model and the context it ran at"
        );

        // No identity attachment (a transcript older than it): the
        // billed id alone, still without the `claude-` prefix.
        let bare = RunModels {
            billed: vec!["claude-opus-5-5".into()],
            identity: None,
        };
        assert_eq!(bare.recorded().as_deref(), Some("opus-5-5"));

        // An identity that is NOT the billed model is not believed: the
        // turns are what was billed.
        let disagree = RunModels {
            billed: vec!["claude-opus-5-5".into()],
            identity: Some("claude-opus-5[1m]".into()),
        };
        assert_eq!(disagree.recorded().as_deref(), Some("opus-5-5"));

        // Two billed models cannot be priced at one row's rates. Both
        // are named, and no row names the pair, so the run reads as
        // unpriced rather than wholly priced at either one.
        let two = RunModels {
            billed: vec!["claude-opus-5-5".into(), "claude-haiku-4-5".into()],
            identity: Some("claude-opus-5-5[1m]".into()),
        };
        assert_eq!(two.recorded().as_deref(), Some("opus-5-5+haiku-4-5"));

        assert_eq!(RunModels::default().recorded(), None, "nothing said");
    }

    /// A DATED SNAPSHOT ID IS ITS CARD ROW (backlog 8e1a2a6f). Haiku
    /// turns are billed as `claude-haiku-4-5-20251001`, and the spelling
    /// kept the date, so the record said `haiku-4-5-20251001` — a model
    /// no row names — and every Haiku run read as unpriced.
    #[test]
    fn a_dated_model_id_is_spelled_as_its_card_row() {
        assert_eq!(card_spelling("claude-haiku-4-5-20251001"), "haiku-4-5");
        assert_eq!(
            card_spelling("claude-haiku-4-5-20251001[1m]"),
            "haiku-4-5[1m]",
            "the context suffix survives the date"
        );
        // Undated ids are untouched: a version number is not a date.
        assert_eq!(card_spelling("claude-opus-5-5"), "opus-5-5");
        assert_eq!(card_spelling("claude-opus-5-5[1m]"), "opus-5-5[1m]");
        assert_eq!(card_spelling("claude-fable-5-1"), "fable-5-1");
        // Only an eight-digit trailing segment is a date.
        assert_eq!(card_spelling("claude-opus-4-1234567"), "opus-4-1234567");

        let haiku = RunModels {
            billed: vec!["claude-haiku-4-5-20251001".into()],
            identity: Some("claude-haiku-4-5".into()),
        };
        assert_eq!(haiku.recorded().as_deref(), Some("haiku-4-5"));
        // The identity names the same model undated: it is believed for
        // its context suffix.
        let long = RunModels {
            billed: vec!["claude-haiku-4-5-20251001".into()],
            identity: Some("claude-haiku-4-5[1m]".into()),
        };
        assert_eq!(long.recorded().as_deref(), Some("haiku-4-5[1m]"));
        // A dated id and its undated twin are one model, not a pair.
        let twins = RunModels {
            billed: vec![
                "claude-haiku-4-5".into(),
                "claude-haiku-4-5-20251001".into(),
            ],
            identity: None,
        };
        assert_eq!(twins.recorded().as_deref(), Some("haiku-4-5"));
    }

    #[test]
    fn a_metered_run_carries_the_models_its_transcript_names() {
        let root = boss_testing::scratch_dir("transcript-usage-model");
        let text = format!(
            "{}\n{OPUS_5_5}",
            r#"{"type":"user","message":{"content":"agent-run r-1"}}"#
        );
        let path = write(&root, "s/subagents/agent-m.jsonl", &text);
        let got = meter(&path, "r-1", &Slice::default()).expect("named");
        assert_eq!(got.models.recorded().as_deref(), Some("opus-5-5[1m]"));
        assert_eq!(got.usage.turns, 3, "the synthetic line is a turn of zero");
        assert_eq!(got.usage.output, 16);
    }

    /// ONE AGENT, TWO RUNS, ONE TRANSCRIPT (backlog 4f74727b) — the
    /// shape of the triage batch measured 2026-09-28 on
    /// `agent-abb03ad066e8e7222.jsonl`: every run named up front, the
    /// model attachment once at the top, and each run's `--report` run
    /// by the agent itself between its turns. Four reports from that
    /// file recorded $0.93, $1.14, $1.49, $1.66 — each the transcript's
    /// running total — so the table summed $5.21 for about $1.66.
    const BATCH: &str = concat!(
        r#"{"type":"user","timestamp":"2026-09-28T09:52:14.973Z","message":{"content":"agent-run r-1 then agent-run r-2"}}"#,
        "\n",
        r#"{"type":"attachment","timestamp":"2026-09-28T09:52:15.581Z","attachment":{"type":"model","identity":{"modelId":"claude-opus-5-5[1m]"}}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-09-28T09:52:17.156Z","message":{"id":"msg_1","model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000,"output_tokens":5}}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-09-28T09:52:18.000Z","message":{"id":"msg_1","model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000,"output_tokens":50}}}"#,
        "\n",
        r#"{"type":"user","timestamp":"2026-09-28T09:52:20.000Z","message":{"content":[{"type":"tool_result"}]}}"#,
        "\n",
        // The turn that runs r-1's `--report`; the report finishes at
        // 09:56:30, and its answer is the next line.
        r#"{"type":"assistant","timestamp":"2026-09-28T09:55:28.861Z","message":{"id":"msg_2","model":"claude-opus-5-5","usage":{"input_tokens":1,"cache_creation_input_tokens":10,"cache_read_input_tokens":2000,"output_tokens":30}}}"#,
        "\n",
        r#"{"type":"user","timestamp":"2026-09-28T09:56:31.000Z","message":{"content":[{"type":"tool_result"}]}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-09-28T09:56:33.475Z","message":{"id":"msg_3","model":"claude-opus-5-5","usage":{"input_tokens":3,"cache_creation_input_tokens":20,"cache_read_input_tokens":3000,"output_tokens":40}}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-09-28T09:57:45.246Z","message":{"id":"msg_4","model":"claude-opus-5-5","usage":{"input_tokens":1,"cache_creation_input_tokens":5,"cache_read_input_tokens":3100,"output_tokens":60}}}"#,
        "\n",
    );

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn two_runs_on_one_transcript_each_meter_only_their_own_turns() {
        let root = boss_testing::scratch_dir("transcript-usage-batch");
        let path = write(&root, "s/subagents/agent-abb.jsonl", BATCH);

        // r-1: from its start to its own report.
        let first = Slice::bounded(
            Some(at("2026-09-28T09:52:00Z")),
            vec![],
            at("2026-09-28T09:56:30Z"),
        );
        let one = meter(&path, "r-1", &first).expect("r-1 metered");
        assert_eq!(one.usage.turns, 2);
        assert_eq!(one.usage.output, 50 + 30);
        assert_eq!(one.usage.cache_read, 1000 + 2000);
        assert_eq!(
            (one.slice.first_line, one.slice.last_line),
            (Some(1), Some(6))
        );

        // r-2: started at the same instant — the batch dispatched every
        // run before the agent began — so its start separates nothing;
        // r-1's report is the bound that does.
        let row = json!([{
            "run_id": "r-1",
            "finished_at": "2026-09-28T09:56:30Z",
            "detail": { "metered": { "transcript": path.display().to_string() } },
        }]);
        let earlier = earlier_reports(
            row.as_array().unwrap(),
            &path,
            "r-2",
            at("2026-09-28T09:58:00Z"),
        );
        let second = Slice::bounded(
            Some(at("2026-09-28T09:52:00Z")),
            earlier,
            at("2026-09-28T09:58:00Z"),
        );
        assert_eq!(second.after, Some(at("2026-09-28T09:56:30Z")));
        assert_eq!(second.after_by.as_deref(), Some("report of run r-1"));
        let two = meter(&path, "r-2", &second).expect("r-2 metered");
        assert_eq!(two.usage.turns, 2, "r-1's two turns are not r-2's");
        assert_eq!(two.usage.output, 40 + 60);
        assert_eq!(two.usage.cache_read, 3000 + 3100);
        assert_eq!(
            (two.slice.first_line, two.slice.last_line),
            (Some(7), Some(9))
        );
        assert_eq!(
            two.models.recorded().as_deref(),
            Some("opus-5-5[1m]"),
            "the session's model attachment belongs to every slice of it"
        );
        assert_eq!(two.slice.zero_reason, None);

        // CONSERVATION: the two slices sum to the whole, not to more.
        let whole = sum_usage(BATCH).unwrap();
        assert_eq!(one.usage.total() + two.usage.total(), whole.total());
        assert_eq!(one.usage.turns + two.usage.turns, whole.turns);
    }

    #[test]
    fn a_turn_is_judged_by_its_first_line_so_no_turn_is_split_between_runs() {
        // msg_1 streams across the bound: its first line is before it,
        // its second after. The turn is r-1's whole, and none of r-2's.
        let bound = at("2026-09-28T09:52:17.500Z");
        let before = cut(BATCH, &Slice::bounded(None, vec![], bound)).0;
        let after = cut(
            BATCH,
            &Slice::bounded(Some(bound), vec![], at("2026-09-28T10:00:00Z")),
        )
        .0;
        assert_eq!(sum_usage(&before).unwrap().output, 50);
        assert!(!after.contains("msg_1"), "{after}");
    }

    #[test]
    fn an_empty_slice_is_a_metered_zero_that_says_why() {
        let root = boss_testing::scratch_dir("transcript-usage-empty");
        let path = write(&root, "s/subagents/agent-abb.jsonl", BATCH);
        // Every turn is already claimed by an earlier report.
        let late = Slice::bounded(
            Some(at("2026-09-28T09:58:00Z")),
            vec![],
            at("2026-09-28T09:59:00Z"),
        );
        let got = meter(&path, "r-2", &late).expect("a measured zero");
        assert_eq!(got.usage, Usage::default());
        let why = got.slice.zero_reason.expect("the zero names its reason");
        assert!(why.contains("no billed turn"), "{why}");

        // A transcript named outright that never names the run holds
        // none of its turns: a zero, and the reason, rather than the
        // whole of another run's spend.
        let got = meter(&path, "r-7", &Slice::default()).expect("a measured zero");
        assert_eq!(got.usage, Usage::default());
        assert_eq!(got.models, RunModels::default());
        let why = got.slice.zero_reason.expect("the zero names its reason");
        assert!(why.contains("never names"), "{why}");

        // A file with no billed turn at all is still not a transcript
        // to meter from — no count, not a zero.
        let empty = write(&root, "s/subagents/agent-none.jsonl", "agent-run r-1\n");
        assert!(meter(&empty, "r-1", &Slice::default()).is_err());
    }

    #[test]
    fn the_earlier_report_is_the_latest_other_run_metered_from_the_same_file() {
        let path = PathBuf::from("/h/.claude/projects/p/s/subagents/agent-abb.jsonl");
        let row = |run: &str, fin: &str, transcript: Option<&str>| match transcript {
            Some(t) => {
                json!({"run_id": run, "finished_at": fin, "detail": {"metered": {"transcript": t}}})
            }
            None => json!({"run_id": run, "finished_at": fin, "detail": {}}),
        };
        let same = "/h/.claude/projects/p/s/subagents/agent-abb.jsonl";
        let rows = vec![
            row("r-0", "2026-09-28T09:40:00Z", Some(same)),
            row("r-1", "2026-09-28T09:56:30Z", Some(same)),
            // Another transcript, an unmetered row, this run's own row
            // (a retried report), and a report after this one's instant.
            row(
                "r-x",
                "2026-09-28T09:57:00Z",
                Some("/h/.claude/projects/p/s/subagents/agent-zzz.jsonl"),
            ),
            row("r-y", "2026-09-28T09:57:01Z", None),
            row("r-2", "2026-09-28T09:57:10Z", Some(same)),
            row("r-3", "2026-09-28T09:59:00Z", Some(same)),
        ];
        let earlier = earlier_reports(&rows, &path, "r-2", at("2026-09-28T09:58:00Z"));
        assert_eq!(
            earlier,
            vec![
                (at("2026-09-28T09:40:00Z"), "r-0".to_string()),
                (at("2026-09-28T09:56:30Z"), "r-1".to_string()),
            ]
        );
        assert_eq!(
            earlier_reports(&rows[2..4], &path, "r-2", at("2026-09-28T09:58:00Z")),
            vec![]
        );
        // The latest of them bounds the slice.
        let s = Slice::bounded(None, earlier, at("2026-09-28T09:58:00Z"));
        assert_eq!(s.after, Some(at("2026-09-28T09:56:30Z")));
        assert_eq!(s.after_by.as_deref(), Some("report of run r-1"));

        // The run's own start wins when it is the later bound.
        let s = Slice::bounded(
            Some(at("2026-09-28T09:57:00Z")),
            vec![(at("2026-09-28T09:56:30Z"), "r-1".into())],
            at("2026-09-28T09:58:00Z"),
        );
        assert_eq!(s.after, Some(at("2026-09-28T09:57:00Z")));
        assert_eq!(s.after_by.as_deref(), Some("run start"));
    }

    /// ONE AGENT, TWO RUNS, NO REPORT BETWEEN THEM (backlog 11a0998a) —
    /// the batch shape 4f74727b's slice could not split: both runs
    /// dispatched before the agent began, a batch prompt naming both
    /// ids at once, each run's prompt READ when its item starts (the
    /// read's tool result is where its marker lands), and the parent
    /// reporting both after the one handback. Report instants separate
    /// nothing here; the markers do.
    const MARKED: &str = concat!(
        r#"{"type":"user","timestamp":"2026-09-28T13:00:00.000Z","message":{"content":"Batch: agent-run r-1 then agent-run r-2; read each prompt file as you start it"}}"#,
        "\n",
        r#"{"type":"attachment","timestamp":"2026-09-28T13:00:00.500Z","attachment":{"type":"model","identity":{"modelId":"claude-opus-5-5[1m]"}}}"#,
        "\n",
        // The turn that reads r-1's prompt, and the read's answer.
        r#"{"type":"assistant","timestamp":"2026-09-28T13:00:02.000Z","message":{"id":"msg_1","model":"claude-opus-5-5","usage":{"input_tokens":1,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000,"output_tokens":10}}}"#,
        "\n",
        r#"{"type":"user","timestamp":"2026-09-28T13:00:03.000Z","message":{"content":[{"type":"tool_result","content":"== THE RUN ==\nrun-marker: agent-run r-1 begins here\n"}]}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-09-28T13:01:00.000Z","message":{"id":"msg_2","model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_creation_input_tokens":200,"cache_read_input_tokens":2000,"output_tokens":20}}}"#,
        "\n",
        // The agent's own text quoting r-2's marker is not a marker: an
        // assistant line is the agent speaking, not a prompt arriving.
        r#"{"type":"assistant","timestamp":"2026-09-28T13:02:00.000Z","message":{"id":"msg_3","model":"claude-opus-5-5","content":[{"type":"text","text":"next: run-marker: agent-run r-2 begins here"}],"usage":{"input_tokens":3,"cache_creation_input_tokens":300,"cache_read_input_tokens":3000,"output_tokens":30}}}"#,
        "\n",
        r#"{"type":"user","timestamp":"2026-09-28T13:02:05.000Z","message":{"content":[{"type":"tool_result","content":"== THE RUN ==\nrun-marker: agent-run r-2 begins here\n"}]}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-09-28T13:03:00.000Z","message":{"id":"msg_4","model":"claude-opus-5-5","usage":{"input_tokens":4,"cache_creation_input_tokens":400,"cache_read_input_tokens":4000,"output_tokens":40}}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-09-28T13:04:00.000Z","message":{"id":"msg_5","model":"claude-opus-5-5","usage":{"input_tokens":5,"cache_creation_input_tokens":500,"cache_read_input_tokens":5000,"output_tokens":50}}}"#,
        "\n",
    );

    #[test]
    fn a_marker_is_read_only_off_a_line_that_arrived_and_only_once_per_run() {
        let got = markers(MARKED);
        assert_eq!(
            got,
            vec![
                Mark {
                    at: at("2026-09-28T13:00:03Z"),
                    run: "r-1".into()
                },
                Mark {
                    at: at("2026-09-28T13:02:05Z"),
                    run: "r-2".into()
                },
            ],
            "the assistant's quotation at 13:02:00 is not r-2's marker"
        );
        assert!(markers(BATCH).is_empty(), "no marker, none read");
        // A placeholder is not an id, and a phrase without its ending is
        // not the marker.
        let prose = concat!(
            r#"{"type":"user","timestamp":"2026-09-28T13:00:00Z","message":{"content":"run-marker: agent-run <id> begins here; run-marker: agent-run r-9 starts"}}"#,
            "\n"
        );
        assert!(markers(prose).is_empty(), "{:?}", markers(prose));
        assert_eq!(marker("r-1"), "run-marker: agent-run r-1 begins here");
        assert!(
            marker("r-1").contains(&needle("r-1")),
            "a marked transcript is found by the run's own phrase"
        );
    }

    /// The report the PARENT runs for each run after one handback, in
    /// either order: each report's instant is after the file's last line.
    #[test]
    fn two_marked_runs_with_no_report_between_them_each_meter_their_own_turns() {
        let root = boss_testing::scratch_dir("transcript-usage-marked");
        let path = write(&root, "s/subagents/agent-mark.jsonl", MARKED);
        let started = Some(at("2026-09-28T12:59:00Z"));
        let (first_report, second_report) =
            (at("2026-09-28T13:10:00Z"), at("2026-09-28T13:10:30Z"));
        let row = |run: &str, fin: &str| {
            json!({
                "run_id": run, "finished_at": fin,
                "detail": { "metered": { "transcript": path.display().to_string() } },
            })
        };
        let whole = sum_usage(MARKED).unwrap();

        for order in [["r-1", "r-2"], ["r-2", "r-1"]] {
            let one = meter(
                &path,
                order[0],
                &Slice::bounded(started, vec![], first_report),
            )
            .expect("metered");
            let rows = vec![row(order[0], "2026-09-28T13:10:00Z")];
            let earlier = earlier_reports(&rows, &path, order[1], second_report);
            assert!(!earlier.is_empty(), "the first report bounds the second");
            let two = meter(
                &path,
                order[1],
                &Slice::bounded(started, earlier, second_report),
            )
            .expect("metered");
            let (r1, r2) = match order[0] {
                "r-1" => (one, two),
                _ => (two, one),
            };

            // r-1: its own read and its work, and the turn that reads
            // r-2's prompt (it runs before r-2's marker arrives).
            assert_eq!(r1.usage.turns, 3, "{order:?}: {:?}", r1.slice);
            assert_eq!(r1.usage.output, 10 + 20 + 30, "{order:?}");
            assert_eq!(
                r1.slice.until_by.as_deref(),
                Some("marker of run r-2"),
                "{order:?}"
            );
            assert_eq!(r1.slice.until, Some(at("2026-09-28T13:02:05Z")));
            assert_eq!(r1.slice.zero_reason, None, "{order:?}");
            // r-2: from its own marker to its report.
            assert_eq!(r2.usage.turns, 2, "{order:?}: {:?}", r2.slice);
            assert_eq!(r2.usage.output, 40 + 50, "{order:?}");
            assert_eq!(r2.slice.after, Some(at("2026-09-28T13:02:05Z")));
            assert_eq!(r2.slice.after_by.as_deref(), Some("marker of run r-2"));
            assert_eq!(r2.slice.zero_reason, None, "{order:?}");
            assert_eq!(
                r2.models.recorded().as_deref(),
                Some("opus-5-5[1m]"),
                "the session's model attachment belongs to every slice"
            );

            // CONSERVATION, whichever report ran first.
            assert_eq!(
                r1.usage.total() + r2.usage.total(),
                whole.total(),
                "{order:?}"
            );
            assert_eq!(r1.usage.turns + r2.usage.turns, whole.turns, "{order:?}");
        }
    }

    /// A transcript that carries no marker — every one written before
    /// this car — is sliced exactly as 4f74727b sliced it.
    #[test]
    fn a_transcript_with_no_marker_is_sliced_by_report_instants_as_before() {
        let s = Slice::bounded(
            Some(at("2026-09-28T09:52:00Z")),
            vec![(at("2026-09-28T09:56:30Z"), "r-1".into())],
            at("2026-09-28T09:58:00Z"),
        );
        assert_eq!(s.clone().marked(&markers(BATCH), "r-2"), s);
        assert_eq!(s.until_by.as_deref(), Some("this report"));
    }

    /// Markers that arrived TOGETHER separate nothing — a batch prompt
    /// that pastes every run's section at once. Those runs fall back to
    /// the report instants among themselves, so the sum still holds.
    #[test]
    fn markers_that_arrive_together_fall_back_to_the_report_instants() {
        let root = boss_testing::scratch_dir("transcript-usage-comarked");
        let text = MARKED
            .replace(
                "Batch: agent-run r-1 then agent-run r-2; read each prompt file as you start it",
                "run-marker: agent-run r-1 begins here\\nrun-marker: agent-run r-2 begins here",
            )
            .replace("run-marker: agent-run r-1 begins here\\n\"}]", "read\"}]")
            .replace("run-marker: agent-run r-2 begins here\\n\"}]", "read\"}]");
        let path = write(&root, "s/subagents/agent-together.jsonl", &text);
        let got = markers(&text);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].at, got[1].at, "{got:?}");

        let started = Some(at("2026-09-28T12:59:00Z"));
        let one = meter(
            &path,
            "r-1",
            &Slice::bounded(started, vec![], at("2026-09-28T13:10:00Z")),
        )
        .unwrap();
        let rows = vec![json!({
            "run_id": "r-1", "finished_at": "2026-09-28T13:10:00Z",
            "detail": { "metered": { "transcript": path.display().to_string() } },
        })];
        let later = at("2026-09-28T13:10:30Z");
        let earlier = earlier_reports(&rows, &path, "r-2", later);
        let two = meter(&path, "r-2", &Slice::bounded(started, earlier, later)).unwrap();
        let whole = sum_usage(&text).unwrap();
        assert_eq!(
            one.usage.total(),
            whole.total(),
            "the first report, today's way"
        );
        assert_eq!(two.usage, Usage::default(), "and nothing counted twice");
        assert!(two.slice.zero_reason.is_some());
    }

    /// A run with no marker in a transcript that has them — a run
    /// dispatched before this car, reported after it — is cut at the
    /// first marker after its own start, so it cannot count the marked
    /// run's turns as its own.
    #[test]
    fn an_unmarked_run_stops_at_the_first_marker_after_it() {
        let marks = markers(MARKED);
        let s = Slice::bounded(
            Some(at("2026-09-28T12:59:00Z")),
            vec![],
            at("2026-09-28T13:10:00Z"),
        )
        .marked(&marks, "r-0");
        assert_eq!(s.until, Some(at("2026-09-28T13:00:03Z")));
        assert_eq!(s.until_by.as_deref(), Some("marker of run r-1"));
        assert_eq!(s.after, Some(at("2026-09-28T12:59:00Z")));
    }

    fn write(root: &Path, rel: &str, text: &str) -> PathBuf {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn the_one_transcript_naming_the_run_is_found_and_two_are_refused() {
        let root = boss_testing::scratch_dir("transcript-usage-find");
        let mine = write(&root, "-work-boss/s-1/subagents/agent-a1.jsonl", TRANSCRIPT);
        // Another run's transcript, and the PARENT session's own file,
        // which holds the phrase too but is not a subagent transcript.
        write(
            &root,
            "-work-boss/s-1/subagents/agent-a2.jsonl",
            "agent-run r-2",
        );
        write(&root, "-work-boss/s-1.jsonl", "agent-run r-1");
        let epoch = SystemTime::UNIX_EPOCH;
        assert_eq!(find_transcripts(&root, "r-1", epoch), vec![mine.clone()]);

        let found = locate(None, Some(&root), "r-1", epoch).expect("found");
        assert_eq!(found, mine);
        let got = meter(&found, "r-1", &Slice::default()).expect("metered");
        assert_eq!(got.path, mine);
        assert_eq!(got.usage.turns, 2);

        // Written before the run opened: not this run's.
        let later = SystemTime::now() + std::time::Duration::from_secs(3600);
        assert!(find_transcripts(&root, "r-1", later).is_empty());

        write(
            &root,
            "-work-boss/s-2/subagents/agent-b1.jsonl",
            "agent-run r-1",
        );
        let why = locate(None, Some(&root), "r-1", epoch).expect_err("two is ambiguous");
        assert!(why.contains("refusing to guess"), "{why}");
        let why = locate(None, Some(&root), "r-9", epoch).expect_err("none is none");
        assert!(why.contains("--transcript"), "{why}");

        // Named outright, the search is skipped.
        let named = locate(Some(&mine), None, "r-1", epoch).expect("named");
        assert_eq!(
            meter(&named, "r-1", &Slice::default())
                .unwrap()
                .usage
                .output,
            466
        );
    }
}
