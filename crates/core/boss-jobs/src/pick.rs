//! The listing's projection: `pick=<paths>` on a row's metadata and
//! `step_pick=<paths>` on each listed step's, answering the named paths
//! and nothing around them (backlog 1b5a14fe).
//!
//! Measuring the factory needed the timings and verdicts, and the list
//! could hand back only everything or nothing: a month of gate-run
//! timings was ~3,500 detail reads, because the per-check seconds live
//! inside a slim step's `receipt`; 1,000 agent-runs were 96 MB, because
//! each row carries its whole brief and report. A pick is the one cheap
//! read for both, and it is generic — no kind is named here.
//!
//! A path is dot-separated. At an object the LONGEST key prefix that
//! exists is tried first, so a key literally spelled with a dot
//! (`launch.model`, on agent-run steps) is reachable; at a string that
//! holds a JSON object or array the walk continues inside it, because
//! the gate's `receipt` is stored as the runner's document verbatim, in
//! a string. A path that resolves to nothing is absent from the answer,
//! never `null` — null is a value a packet can record.

use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Map, Value};

/// How many paths one parameter may name. A pick is a short list of
/// measurements. This bounds the NUMBER of walks per object, not the
/// cost of one: that is the next two bounds' job.
pub const MAX_PICK_PATHS: usize = 32;

/// How many segments, and how many bytes, one path may hold. At an
/// object the walk tries every key prefix of what remains, so one walk
/// costs about the square of a path's length, and it runs once per row
/// (`pick`) or once per step of every row (`step_pick`), up to 1,000
/// rows. Unbounded, a single 64 KB `step_pick=a.a.a…` measured 1.7 s
/// of CPU per object, hours per request, on a worker of the system of
/// record (reviews 6fd5ad34 and 92043bd0, 2026-10-18); the count bound
/// above, which an earlier comment here credited with preventing it,
/// never touched it. The longest path read today has two segments
/// (`receipt.checks`, `launch.model`).
pub const MAX_PICK_SEGMENTS: usize = 8;
pub const MAX_PICK_PATH_BYTES: usize = 128;

/// Read `<name>=a,b.c` into its paths, or the sentence its 400 carries,
/// which starts with the parameter's name. `None` when it was not sent.
/// An empty list, an empty entry or an empty segment is refused: each
/// would otherwise pick nothing and answer as if the packet held
/// nothing.
pub fn parse(raw: Option<&str>, name: &str) -> Result<Option<Vec<String>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let count = raw.split(',').count();
    if count > MAX_PICK_PATHS {
        return Err(format!(
            "{name}: at most {MAX_PICK_PATHS} paths, got {count}"
        ));
    }
    let paths: Vec<String> = raw.split(',').map(str::to_string).collect();
    // Named by its size, not echoed: the path refused may be 64 KB.
    if let Some(long) = paths
        .iter()
        .find(|p| p.len() > MAX_PICK_PATH_BYTES || p.split('.').count() > MAX_PICK_SEGMENTS)
    {
        return Err(format!(
            "{name}: a path holds at most {MAX_PICK_SEGMENTS} segments and \
             {MAX_PICK_PATH_BYTES} bytes, got one of {} segments and {} bytes",
            long.split('.').count(),
            long.len()
        ));
    }
    match paths
        .iter()
        .find(|p| p.trim() != p.as_str() || p.split('.').any(str::is_empty))
    {
        Some(bad) => Err(format!(
            "{name}: `{bad}` is not a path — expected comma-separated keys, \
             each dot-separated with no empty segment, e.g. {name}=verdict,receipt.checks"
        )),
        None => Ok(Some(paths)),
    }
}

/// The paths of `paths` that resolve in `value`, keyed by the path as
/// asked.
pub fn picked(value: &Value, paths: &[String]) -> Value {
    picked_with(value, paths, &mut Docs::default())
}

/// The documents found inside strings while answering one `picked()`
/// call, each parsed once however many paths, or backtracking attempts,
/// reach it: 32 paths under `receipt.*` re-parsed a gate's 67 KB
/// receipt 32 times per step (review 92043bd0). Keyed by the string's
/// address and length. Every string reached lives in the borrowed
/// value or inside a document this map holds, so none moves or drops
/// while the map lives, and two live non-empty strings never share an
/// address (every empty one parses to nothing alike).
#[derive(Default)]
struct Docs {
    parsed: HashMap<(usize, usize), Option<Rc<Value>>>,
}

impl Docs {
    fn of(&mut self, s: &str) -> Option<Rc<Value>> {
        self.parsed
            .entry((s.as_ptr() as usize, s.len()))
            .or_insert_with(|| {
                serde_json::from_str::<Value>(s)
                    .ok()
                    .filter(|doc| doc.is_object() || doc.is_array())
                    .map(Rc::new)
            })
            .clone()
    }
}

fn picked_with(value: &Value, paths: &[String], docs: &mut Docs) -> Value {
    Value::Object(
        paths
            .iter()
            .filter_map(|p| {
                let segments: Vec<&str> = p.split('.').collect();
                resolve(value, &segments, docs).map(|v| (p.clone(), v))
            })
            .collect::<Map<String, Value>>(),
    )
}

fn resolve(value: &Value, segments: &[&str], docs: &mut Docs) -> Option<Value> {
    if segments.is_empty() {
        return Some(value.clone());
    }
    match value {
        Value::Object(o) => (1..=segments.len()).rev().find_map(|n| {
            o.get(&segments[..n].join("."))
                .and_then(|next| resolve(next, &segments[n..], docs))
        }),
        Value::String(s) => docs.of(s).and_then(|doc| resolve(&doc, segments, docs)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_path_walks_objects_and_json_strings() {
        let doc = json!({"receipt": json!({"checks": [1, 2]}).to_string(), "v": "green"});
        assert_eq!(
            picked(&doc, &paths(&["receipt.checks", "v"])),
            json!({"receipt.checks": [1, 2], "v": "green"})
        );
    }

    #[test]
    fn the_longest_literal_key_wins_and_a_miss_falls_back_to_the_walk() {
        let doc = json!({"a.b": 1, "a": {"b": 2, "c": 3}});
        assert_eq!(picked(&doc, &paths(&["a.b"])), json!({"a.b": 1}));
        assert_eq!(picked(&doc, &paths(&["a.c"])), json!({"a.c": 3}));
    }

    #[test]
    fn a_string_that_is_not_a_document_is_a_leaf() {
        let doc = json!({"s": "plain text", "n": "42"});
        assert_eq!(picked(&doc, &paths(&["s.x", "n.x"])), json!({}));
        assert_eq!(picked(&doc, &paths(&["s"])), json!({"s": "plain text"}));
    }

    #[test]
    fn an_unreadable_list_is_refused_naming_its_parameter() {
        assert_eq!(parse(None, "pick"), Ok(None));
        assert_eq!(parse(Some("a,b.c"), "pick"), Ok(Some(paths(&["a", "b.c"]))));
        for bad in ["", "a,,b", "a..b", ".a", "a.", " a"] {
            let why = parse(Some(bad), "step_pick").expect_err(bad);
            assert!(why.starts_with("step_pick:"), "{why}");
        }
        let many = vec!["k"; MAX_PICK_PATHS + 1].join(",");
        assert!(parse(Some(&many), "pick").is_err());
    }

    /// Reviews 6fd5ad34 and 92043bd0 (2026-10-18) measured one 64 KB
    /// path of 32,000 segments at 1.7 s of CPU per object, quadratic in
    /// its length, so `limit=1000&step_pick=a.a.a…` held a jobs-API
    /// worker for hours. The count bound never touched it.
    #[test]
    fn a_path_is_bounded_in_segments_and_bytes() {
        let deepest = ["a"; MAX_PICK_SEGMENTS].join(".");
        let longest = "k".repeat(MAX_PICK_PATH_BYTES);
        for good in [&deepest, &longest] {
            assert_eq!(parse(Some(good), "pick"), Ok(Some(paths(&[good]))));
        }
        let too_deep = ["a"; MAX_PICK_SEGMENTS + 1].join(".");
        let too_long = "k".repeat(MAX_PICK_PATH_BYTES + 1);
        let the_reviewed_shape = vec!["a"; 32_000].join(".");
        for bad in [too_deep, too_long, the_reviewed_shape] {
            let why = parse(Some(&format!("verdict,{bad}")), "step_pick")
                .expect_err("an over-long path is refused");
            assert!(why.starts_with("step_pick:"), "{why}");
        }
    }

    /// Review 92043bd0: 32 paths under `receipt.*` re-parsed a gate's
    /// 67 KB receipt string 32 times per step. One `picked()` call
    /// parses each JSON-bearing string once, however many paths, or
    /// backtracking attempts, reach it.
    #[test]
    fn a_json_string_is_parsed_once_however_many_paths_read_it() {
        let inner = json!({"x": 1}).to_string();
        let receipt = json!({"checks": [1], "verdict": "green", "inner": inner}).to_string();
        let doc = json!({"receipt": receipt});
        let mut docs = Docs::default();
        let got = picked_with(
            &doc,
            &paths(&[
                "receipt.checks",
                "receipt.verdict",
                "receipt.inner.x",
                "receipt.inner.y",
                "receipt.nowhere.deep",
            ]),
            &mut docs,
        );
        assert_eq!(
            got,
            json!({"receipt.checks": [1], "receipt.verdict": "green", "receipt.inner.x": 1})
        );
        assert_eq!(
            docs.parsed.len(),
            2,
            "the receipt once and its inner document once"
        );
    }
}
