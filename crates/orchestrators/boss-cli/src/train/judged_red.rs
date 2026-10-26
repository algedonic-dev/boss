//! A JUDGED red releases the track at once — the auto-cancel's fast arm.
//!
//! Backlog a2d4d842 (David, 2026-09-24: "we don't want to be in the
//! habit of letting red linger and train us to ignore it"). Two trains
//! that day — f7bd1e9d (17:17) and 02801b05 (18:31) — went red on
//! `CI / web` with the failing check NAMED, their train gates finished
//! red on the same `svelte-check` a quarter of an hour later, and each
//! still held the one track until an operator cancelled it by hand (40
//! and 27 minutes), because [`auto_cancel_reason`] waits out
//! `stall_hours` (6) before it releases anything. The wait exists for a
//! train that may yet be repaired in place or whose verdict is not in;
//! a train whose BOTH halves have spoken and whose red names what failed
//! has nothing left to wait for. The six-hour rule stays as the backstop
//! for every red this does not judge.

use super::*;
use crate::train_gate::Standing;

/// The failing checks a JUDGED red names — Some only when all of it is
/// on the record, else None and the stall rule decides as before:
///
/// - the train has not merged (the rule every cancel keeps);
/// - BOTH halves have spoken: the forge's verdict is `failing` or
///   `green` (not pending, not aborted), and the train gate FINISHED —
///   `Failed` or `Green` (a gate still running, refused, lost or never
///   filed has not judged the assembled tree);
/// - at least one half is red;
/// - no failing forge check declares a refusal — infrastructure is not
///   a verdict (`verdict_strikes_cars`);
/// - the red NAMES what failed: the forge's failing check contexts, and
///   the gate's failed checks off its receipt's `fails` lines. A red
///   that names nothing is not judged here.
///
/// The names come back forge first, then the gate's, deduplicated.
pub(crate) fn judged_red_checks(
    train: &Value,
    forge_verdict: &str,
    gate: Option<&Standing>,
    rollup: Option<&Value>,
    gate_fails: &[String],
) -> Option<Vec<String>> {
    if step_done(find_step(train, "merged", "Merged into main")) {
        return None;
    }
    let forge_red = match forge_verdict {
        "failing" => true,
        "green" => false,
        _ => return None,
    };
    let gate_red = match gate {
        Some(Standing::Failed) => true,
        Some(Standing::Green) => false,
        _ => return None,
    };
    if !(forge_red || gate_red) || any_failing_check_refused(rollup) {
        return None;
    }
    // `failing_checks` answers "unnamed check" for an entry with neither
    // `context` nor `name`; that is the absence of a name, not one.
    let mut named: Vec<String> = if forge_red {
        failing_checks(rollup)
            .into_iter()
            .filter(|c| c != "unnamed check")
            .collect()
    } else {
        Vec::new()
    };
    if gate_red {
        for check in gate_check_names(gate_fails) {
            if !named.contains(&check) {
                named.push(check);
            }
        }
    }
    (!named.is_empty()).then_some(named)
}

/// The check a gate receipt's `fails` line names — the text before its
/// first `:` (`svelte-check: | Error: …`, `test: <name> - FAILED`), as
/// the gate runner writes every rung (infra/gate-runner/run.sh
/// `detail`). A line with no `:` names nothing. In first-seen order,
/// once each.
pub(crate) fn gate_check_names(gate_fails: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in gate_fails {
        let Some((check, _)) = line.split_once(':') else {
            continue;
        };
        let check = check.trim();
        if !check.is_empty() && !out.iter().any(|c| c == check) {
            out.push(check.to_string());
        }
    }
    out
}

/// The reason a judged red's cars carry back to the dock (their
/// `skip_reason`) and the `cancelled` step records: the failing checks
/// by name, and the wait it did not take — so nobody reading a car
/// released in minutes wonders where the six hours went.
pub(crate) fn judged_red_cancel_reason(named: &[String], stall_hours: i64) -> String {
    format!(
        "judged red: {} failed, CI and the train gate both finished — cancelled at the first \
         reconcile after the verdict rather than after the {stall_hours}h stall rule (a2d4d842); \
         cars released to board a later train",
        named.join(", ")
    )
}

/// Every repo file a failure text LOCATES, as `path` from a
/// `path:line:col` the text ties to a FAILURE — never to a warning.
/// Three shapes, each measured on a real red:
///
/// - `panicked at <path>:<line>:<col>` — a Rust test (the gate
///   runner's located rung; [`fails_entry_path`] reads the same);
/// - `--> <path>:<line>:<col>` under an `error…` header — rustc (a
///   `warning` header's arrow is not a failure);
/// - a line that is only `<path>:<line>:<col>`, followed by one that
///   starts `Error` — svelte-check as both the gate and the forge
///   print it (trains f7bd1e9d and 02801b05, 2026-09-24); the same
///   shape followed by `Warn` is a warning and is skipped;
/// - inside a panic's MESSAGE (the panic line's own text and the lines
///   after it, up to cargo's `note:` or the next test), every
///   `<path>:<line>` it names ([`named_location_path`]) — a whole-tree
///   scanner panics in its own file and lists the offending lines in
///   the message, so the panic's location alone blames no car (train
///   22789ff1, 2026-09-26, backlog 4a56c797).
///
/// Forge log lines carry a runner timestamp and ANSI colour inside the
/// path itself (`…/apps/web/\x1b[32msrc/…`), so both come off first.
/// Paths are normalised (`..` resolved) but may stay ABSOLUTE — the
/// gate's `/gate-target/repo/…`, the forge's `/workspace/<owner>/<repo>/…`
/// — and [`cars_to_hold`] matches them to a car's repo-relative files
/// by path suffix. Sorted, once each.
pub(crate) fn located_failures(text: &str) -> Vec<String> {
    let lines: Vec<String> = text.lines().map(clean_log_line).collect();
    let mut out: Vec<String> = Vec::new();
    let mut under_error = false;
    let mut in_panic = false;
    for (i, line) in lines.iter().enumerate() {
        if let Some((_, rest)) = line.split_once("panicked at ") {
            let mut words = rest.split_whitespace();
            if let Some(path) = words
                .next()
                .and_then(|t| location_path(t.strip_suffix(':').unwrap_or(t)))
            {
                out.push(path);
            }
            out.extend(words.filter_map(named_location_path));
            in_panic = true;
            continue;
        }
        if in_panic {
            if PANIC_MESSAGE_ENDS.iter().any(|p| line.starts_with(p)) {
                in_panic = false;
            } else {
                out.extend(line.split_whitespace().filter_map(named_location_path));
                continue;
            }
        }
        if line.starts_with("error") {
            under_error = true;
            continue;
        }
        if line.starts_with("warning") {
            under_error = false;
            continue;
        }
        if let Some(loc) = line.strip_prefix("--> ") {
            if under_error && let Some(path) = location_path(loc.trim()) {
                out.push(path);
            }
            continue;
        }
        if !line.contains(char::is_whitespace)
            && let Some(path) = location_path(line)
            && lines[i + 1..]
                .iter()
                .find(|l| !l.is_empty())
                .is_some_and(|next| next.starts_with("Error"))
        {
            out.push(path);
        }
    }
    let mut out: Vec<String> = out.iter().map(|p| normalise_path(p)).collect();
    out.sort();
    out.dedup();
    out
}

/// One log line as a reader sees it: ANSI escapes removed, a leading
/// runner timestamp (`2026-09-24T17:19:53.3447198Z `) dropped, trimmed.
fn clean_log_line(raw: &str) -> String {
    let mut plain = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
            }
            continue;
        }
        plain.push(c);
    }
    let trimmed = plain.trim();
    let is_stamp = |t: &str| {
        t.len() >= 20
            && t.ends_with('Z')
            && t.as_bytes().get(4) == Some(&b'-')
            && t.contains('T')
            && t.bytes().take(4).all(|b| b.is_ascii_digit())
    };
    match trimmed.split_once(' ') {
        Some((head, rest)) if is_stamp(head) => rest.trim().to_string(),
        None if is_stamp(trimmed) => String::new(),
        _ => trimmed.to_string(),
    }
}

/// The path of a `<path>:<line>:<col>` token, or None when the token is
/// not that shape.
fn location_path(token: &str) -> Option<String> {
    let mut parts = token.rsplitn(3, ':');
    let (col, line, path) = (parts.next()?, parts.next()?, parts.next()?);
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (numeric(col) && numeric(line) && !path.is_empty()).then(|| path.to_string())
}

/// The lines that close a Rust panic's message: cargo's backtrace note,
/// the next test's header, the failures list, the result line, another
/// panic's thread line, or a compiler/runner diagnostic.
const PANIC_MESSAGE_ENDS: &[&str] = &[
    "note:",
    "---- ",
    "failures:",
    "test result:",
    "thread '",
    "error",
    "warning",
];

/// A file a panic MESSAGE names as `<path>:<line>` or
/// `<path>:<line>:<col>` — the shape a whole-tree scanner prints its
/// offenders in (the_sim_bypass_has_one_door: `{rel}:{n}`, train
/// 22789ff1, backlog 4a56c797). Surrounding punctuation comes off; the
/// path must have a directory and a file extension, so prose (`see
/// crates/a/`), a Rust path (`Client::from_env(`) and a URL are not
/// locations. A path without a line number is prose, not a location.
fn named_location_path(word: &str) -> Option<String> {
    let token = word
        .trim_start_matches(['(', '[', '\'', '"', '`'])
        .trim_end_matches([',', ';', '.', ')', ']', ':', '\'', '"', '`']);
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let (head, last) = token.rsplit_once(':')?;
    if !numeric(last) {
        return None;
    }
    let path = match head.rsplit_once(':') {
        Some((p, line)) if numeric(line) => p,
        _ => head,
    };
    let file = path.rsplit_once('/')?.1;
    (!path.contains("://") && !path.contains(':') && file.contains('.') && !file.ends_with('.'))
        .then(|| path.to_string())
}

/// `a/b/../c` → `a/c`, keeping a leading `/`.
fn normalise_path(path: &str) -> String {
    let mut segs: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    let joined = segs.join("/");
    if path.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Every file the verdict locates, across both halves: the gate
/// receipt's `fails` lines and `fails_excerpt`, and the log tail the
/// forge attached to each FAILING check. Sorted, once each.
pub(crate) fn verdict_located_files(
    gate_fails: &[String],
    gate_excerpt: &[(String, String)],
    rollup: Option<&Value>,
) -> Vec<String> {
    let forge_tails = rollup
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| c.get("conclusion").and_then(Value::as_str) == Some("FAILURE"))
        .filter_map(|c| c.get("log_tail").and_then(Value::as_str));
    let mut out: Vec<String> = gate_fails
        .iter()
        .map(String::as_str)
        .chain(gate_excerpt.iter().map(|(_, text)| text.as_str()))
        .chain(forge_tails)
        .flat_map(located_failures)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Does a located path name this repo-relative file? Equal, or — for a
/// file below the repo root — the located path ends in `/<file>`. A
/// top-level file matches only itself: a car's `Cargo.toml` is not
/// `/gate-target/repo/crates/x/Cargo.toml`.
pub(crate) fn names_file(located: &str, file: &str) -> bool {
    located == file
        || (file.contains('/')
            && located
                .strip_suffix(file)
                .is_some_and(|head| head.ends_with('/')))
}

/// Which cars the verdict names: for each located file, the ONE car
/// aboard that changed it — a file two cars changed names neither, and
/// a file no car changed names nobody (the release is all that red
/// earns). `(car id, the repo files only it changed that the verdict
/// located)`, in the order the cars are given.
pub(crate) fn cars_to_hold(
    located: &[String],
    car_files: &[(String, Vec<String>)],
) -> Vec<(String, Vec<String>)> {
    let mut held: Vec<(String, Vec<String>)> = Vec::new();
    for loc in located {
        let owners: Vec<(&String, &String)> = car_files
            .iter()
            .filter_map(|(car, files)| files.iter().find(|f| names_file(loc, f)).map(|f| (car, f)))
            .collect();
        let [(car, file)] = owners.as_slice() else {
            continue;
        };
        match held.iter_mut().find(|(c, _)| c == *car) {
            Some((_, files)) => {
                if !files.contains(file) {
                    files.push((*file).clone());
                }
            }
            None => held.push(((*car).clone(), vec![(*file).clone()])),
        }
    }
    held.sort_by_key(|(car, _)| car_files.iter().position(|(c, _)| c == car));
    held
}

/// The hold a named car carries on its review step — the dock brake
/// `boss hold` writes, so `boss release` takes it off. It says which
/// train, which check, which file, and what to do, because the next
/// reader is whoever is rebuilding the car.
///
/// `files` empty means the car was named as the ONLY car aboard
/// ([`culprit_cars`]) rather than by a file, and the reason says so.
/// `failure_line` is the verdict's first failing line
/// ([`first_failure_line`]), so the rebuilder reads what failed without
/// opening the train (b4a95f47).
pub(crate) fn judged_red_hold_reason(
    train_id: &str,
    named: &[String],
    files: &[String],
    failure_line: Option<&str>,
) -> String {
    let why = if files.is_empty() {
        "as the only car aboard".to_string()
    } else {
        format!(
            "in {}, a file only this car changed in that consist",
            files.join(", ")
        )
    };
    let line = failure_line
        .map(|l| format!(" — first failing line: {l}"))
        .unwrap_or_default();
    format!(
        "held by the conductor: train {} was judged red ({}) {why}{line} — rebuild it on current \
         main, re-gate, then `boss release` it (a2d4d842, b4a95f47)",
        id8(train_id),
        named.join(", "),
    )
}

/// The forge's failing checks when forge CI ALONE has judged the train
/// red — the half [`judged_red_checks`] cannot judge (backlog b4a95f47).
/// A red forge settles the verdict on its own (`combined_verdict`), the
/// `ci` step is judged in that pass, and a judged train never launches
/// its gate — so a train whose gate was not filed before the forge went
/// red (train 496122a0: the cluster at its gate bound) waits for a half
/// that will never speak, and only the six-hour stall rule released it.
/// Some only when:
///
/// - the train has not merged;
/// - the forge's verdict is `failing`;
/// - the train gate has NOT finished (`Failed`/`Green` is the
///   both-halves rule's to judge, with the gate's receipt to locate by);
/// - no failing forge check declares a refusal;
/// - the forge names at least one failing check.
///
/// This judges the red; it does not name a culprit. The conductor
/// cancels on it only when [`culprit_cars`] names one, and a red that
/// names none keeps today's path: the alert, then the stall rule.
pub(crate) fn forge_judged_red_checks(
    train: &Value,
    forge_verdict: &str,
    gate: Option<&Standing>,
    rollup: Option<&Value>,
) -> Option<Vec<String>> {
    if step_done(find_step(train, "merged", "Merged into main"))
        || forge_verdict != "failing"
        || matches!(gate, Some(Standing::Failed) | Some(Standing::Green))
        || any_failing_check_refused(rollup)
    {
        return None;
    }
    let named: Vec<String> = failing_checks(rollup)
        .into_iter()
        .filter(|c| c != "unnamed check")
        .collect();
    (!named.is_empty()).then_some(named)
}

/// The cars a judged red names as its culprit: each car that alone
/// changed a file the verdict locates ([`cars_to_hold`]) — or, when no
/// file names one and exactly ONE car is aboard, that car with no files.
/// A train is its cars on a main that was green when they boarded, so a
/// sole car is the one change the red can be about (train 496122a0: the
/// failing test read ci.yml, which the car changed, while the log
/// located only the test, which nobody did). Two or more cars and no
/// file is no culprit; an empty list is no culprit.
///
/// `car_files` MUST be the whole consist — every id in the train's
/// `boarded_jobs`, open or not — never the cars still open for it: a car
/// closed mid-ride is still in the tree that was judged, and dropping it
/// made the other car read as the only one aboard (review 38f094c5).
pub(crate) fn culprit_cars(
    located: &[String],
    car_files: &[(String, Vec<String>)],
) -> Vec<(String, Vec<String>)> {
    let held = cars_to_hold(located, car_files);
    match car_files {
        [(sole, _)] if held.is_empty() => vec![(sole.clone(), Vec::new())],
        _ => held,
    }
}

/// The `cancelled` step's reason for a forge-judged red with a named
/// culprit — None when [`culprit_cars`] named nobody, which is the
/// multi-car red that pages and waits as before. It says which checks,
/// which car and why that car, and which wait it did not take.
///
/// It is written BEFORE the best-effort hold runs, and on whichever pass
/// names the culprit — the first after the verdict or a later one — so
/// it states the decision, never that the hold landed or which pass it
/// was: the culprit's own review step records whether it was held
/// (review 38f094c5).
pub(crate) fn forge_culprit_cancel_reason(
    named: &[String],
    culprits: &[(String, Vec<String>)],
    stall_hours: i64,
) -> Option<String> {
    if culprits.is_empty() {
        return None;
    }
    let who: Vec<String> = culprits
        .iter()
        .map(|(car, files)| {
            if files.is_empty() {
                format!("car {}, the only car aboard", id8(car))
            } else {
                format!("car {}, which alone changed {}", id8(car), files.join(", "))
            }
        })
        .collect();
    Some(format!(
        "judged red on forge CI: {} failed and names its culprit ({}) — cancelled on the \
         reconcile that named it, without waiting for the train gate (a train judged on CI \
         alone launches none) or the {stall_hours}h stall rule (b4a95f47); the conductor holds \
         the culprit if it is still open for this train (its review step records whether the \
         hold was written) and releases every other car to board a later train",
        named.join(", "),
        who.join("; ")
    ))
}

/// The bound on [`first_failure_line`]: one line of a hold reason, not a
/// log.
pub(crate) const FAILURE_LINE_BYTES: usize = 240;

/// The verdict's first failing LINE, for a hold reason: in the first
/// failing forge check's log tail, the first line (timestamp and colour
/// off) that starts `error`/`Error` or carries a failure marker
/// (`FAILURE_MARKERS`), prefixed with its check; failing that, the gate
/// receipt's first `fails` line, which already names its check. Bounded
/// to [`FAILURE_LINE_BYTES`]. None when neither half recorded a line.
pub(crate) fn first_failure_line(rollup: Option<&Value>, gate_fails: &[String]) -> Option<String> {
    let forge = failing_check_logs(rollup)
        .into_iter()
        .find_map(|(check, tail)| {
            tail.lines()
                .map(clean_log_line)
                .find(|l| {
                    l.starts_with("error")
                        || l.starts_with("Error")
                        || FAILURE_MARKERS.iter().any(|m| l.contains(m))
                })
                .map(|l| format!("{check}: {l}"))
        });
    let mut line = forge.or_else(|| gate_fails.first().cloned())?;
    if line.len() > FAILURE_LINE_BYTES {
        let mut cut = FAILURE_LINE_BYTES;
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line.truncate(cut);
        line.push('…');
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- the two real trains of 2026-09-24 ----------------------------------
    //
    // Both fixtures are the record's own shapes: the forge rollup's four
    // checks as the `ci` step stamped them, the failing check's log tail
    // as the forge returned it (ANSI colour and runner timestamps
    // included), and the gate-run receipt's `fails` lines and
    // `fails_excerpt` as the runner wrote them. Trimmed, never retyped.

    const F7BD_ID: &str = "f7bd1e9d-e070-4c06-8852-a19a4902a682";
    const O280_ID: &str = "02801b05-ae47-476a-b08d-2cd979ab1978";

    fn train(id: &str, merged: &str) -> Value {
        json!({
            "id": id,
            "status": "open",
            "metadata": {},
            "steps": [
                {"spec_slug": "ci", "title": "Yard inspection — CI verdict", "status": "completed",
                 "metadata": {"result": "failing", "completed_at": "2026-09-24T17:20:32Z"}},
                {"spec_slug": "merged", "title": "DEPARTED — merged into main", "status": merged,
                 "metadata": {}}
            ]
        })
    }

    /// train f7bd1e9d's forge rollup: `CI / web` red, its log tail
    /// locating the error the way svelte-check prints it through the
    /// forge runner.
    fn f7bd_rollup() -> Value {
        json!([
            {"context": "CI / build-image (pull_request)", "conclusion": "SUCCESS", "description": ""},
            {"context": "CI / locomotive (pull_request)", "conclusion": "SUCCESS", "description": ""},
            {"context": "CI / web (pull_request)", "conclusion": "FAILURE", "description": "",
             "log_tail": "…(log tail — earlier lines omitted)\n\
        2026-09-24T17:19:53.3447098Z     expect(got.source).toBe('server');\u{1b}[39m\n\
        2026-09-24T17:19:53.3447148Z \n\
        2026-09-24T17:19:53.3447198Z /workspace/david/boss/apps/web/\u{1b}[32msrc/it/yard/phone-strip.test.ts\u{1b}[39m:101:32\n\
        2026-09-24T17:19:53.3448213Z \u{1b}[31mError\u{1b}[39m: Conversion of type '{ thirds: { third: string; regions: string[]; }[]; }' may be a mistake\n\
        2026-09-24T17:19:53.3448738Z \n\
        2026-09-24T17:19:53.3449978Z /workspace/david/boss/apps/web/\u{1b}[32m../../libs/web-kit/src/ui/Tabs.svelte\u{1b}[39m:20:1\n\
        2026-09-24T17:19:53.3450155Z \u{1b}[33mWarn\u{1b}[39m: Non-interactive element `<nav>` cannot have interactive role 'tablist'\n\
        2026-09-24T17:19:53.3453744Z \u{1b}[31msvelte-check found 3 errors and 74 warnings in 29 files\n\
        2026-09-24T17:19:53.4273826Z \u{1b}[39merror: script \"typecheck\" exited with code 1\n\
        2026-09-24T17:19:54.1584137Z Job 'web' failed"},
            {"context": "CI / reclaim (pull_request)", "conclusion": "SUCCESS", "description": ""}
        ])
    }

    /// gate-run c9aa4dac's receipt `fails`, verbatim in shape.
    fn f7bd_gate_fails() -> Vec<String> {
        vec![
            "svelte-check: no cargo test failure in this check's output; 4 error line(s), first 4:".to_string(),
            "svelte-check: | Error: Type '{ window_hours: number; }' is not assignable to type 'Readonly<{ window_hours: number; }>' (+236 char(s); the Job log replay has the full text)".to_string(),
            "svelte-check: | error: script \"typecheck\" exited with code 1".to_string(),
        ]
    }

    /// gate-run c9aa4dac's receipt `fails_excerpt`: the first error's
    /// location fell in the omitted lead-in, the next two are located,
    /// and a WARNING's location sits beside them.
    fn f7bd_gate_excerpt() -> Vec<(String, String)> {
        vec![(
            "svelte-check".to_string(),
            "... (249 line(s) before the first failure marker omitted; the Job log replay has them)\n\
Error: Type '{ window_hours: number; }' is not assignable to type 'Readonly<{ window_hours: number; }>'.\n\
\n\
/gate-target/repo/apps/web/src/it/yard/phone-strip.test.ts:83:26\n\
Error: Conversion of type '{ thirds: { third: string; regions: string[]; }[]; }' may be a mistake\n\
    const got = thirdsOf({ ...regions(), thirds: served } as Regions);\n\
\n\
/gate-target/repo/apps/web/src/it/yard/phone-strip.test.ts:101:32\n\
Error: Conversion of type '{ thirds: { third: string; regions: string[]; }[]; }' may be a mistake\n\
\n\
/gate-target/repo/apps/web/../../libs/web-kit/src/ui/Tabs.svelte:20:1\n\
Warn: Non-interactive element `<nav>` cannot have interactive role 'tablist'\n\
\n\
====================================\n\
svelte-check found 3 errors and 74 warnings in 29 files\n\
error: script \"typecheck\" exited with code 1"
                .to_string(),
        )]
    }

    /// What each car aboard f7bd1e9d changed: car G (6c7c75f7,
    /// feat/it-phone-strip-map) owns the failing test file; the shed
    /// car (f13b5fd0) touched Rust only.
    fn f7bd_car_files() -> Vec<(String, Vec<String>)> {
        vec![
            (
                "6c7c75f7-4f34-4f5f-87c0-3737e92a66ed".to_string(),
                vec![
                    "apps/web/src/it/yard/PhoneStrip.svelte".to_string(),
                    "apps/web/src/it/yard/phone-strip.test.ts".to_string(),
                    "apps/web/src/it/yard/phone-strip.ts".to_string(),
                ],
            ),
            (
                "f13b5fd0-d15c-4c89-b3a2-3944b7b3dc52".to_string(),
                vec![
                    "crates/core/boss-jobs/src/car.rs".to_string(),
                    "crates/core/boss-jobs/src/regions.rs".to_string(),
                    "crates/orchestrators/boss-cli/src/orient.rs".to_string(),
                ],
            ),
        ]
    }

    #[test]
    fn train_f7bd1e9d_both_halves_red_and_named_is_judged() {
        let rollup = f7bd_rollup();
        let named = judged_red_checks(
            &train(F7BD_ID, "ready"),
            "failing",
            Some(&Standing::Failed),
            Some(&rollup),
            &f7bd_gate_fails(),
        )
        .expect("CI / web red and the train gate finished red: judged");
        assert_eq!(
            named,
            vec![
                "CI / web (pull_request)".to_string(),
                "svelte-check".to_string()
            ]
        );
        let reason = judged_red_cancel_reason(&named, 6);
        assert!(
            reason.contains("CI / web (pull_request)") && reason.contains("svelte-check"),
            "the reason names both halves' failing checks: {reason}"
        );
        assert!(
            reason.contains("6h"),
            "the reason says which wait it did not take: {reason}"
        );
    }

    /// 17:20Z to ~17:35Z on 2026-09-24: CI red, the train gate still
    /// running. Not judged yet — the gate finishes in minutes and its
    /// receipt is what locates the failure for the hold.
    #[test]
    fn a_red_forge_with_the_gate_still_running_is_not_yet_judged() {
        let rollup = f7bd_rollup();
        for gate in [
            None,
            Some(Standing::Pending),
            Some(Standing::Lost),
            Some(Standing::Refused("disk floor".into())),
            Some(Standing::Unavailable("no kubectl".into())),
        ] {
            assert_eq!(
                judged_red_checks(
                    &train(F7BD_ID, "ready"),
                    "failing",
                    gate.as_ref(),
                    Some(&rollup),
                    &[],
                ),
                None,
                "gate {gate:?} has not judged the assembled tree"
            );
        }
    }

    #[test]
    fn a_gate_red_with_the_forge_still_running_is_not_yet_judged() {
        assert_eq!(
            judged_red_checks(
                &train(O280_ID, "ready"),
                "pending",
                Some(&Standing::Failed),
                None,
                &f7bd_gate_fails(),
            ),
            None
        );
        assert_eq!(
            judged_red_checks(
                &train(O280_ID, "ready"),
                "aborted",
                Some(&Standing::Failed),
                None,
                &f7bd_gate_fails(),
            ),
            None
        );
    }

    /// Train #361's shape (2026-09-14): CI green, the train gate red and
    /// naming its check. Both halves spoke; the red is named.
    #[test]
    fn a_green_forge_and_a_named_red_gate_is_judged() {
        let green = json!([{"context": "CI / web", "conclusion": "SUCCESS"}]);
        let fails =
            vec!["test: the_real_run_refuses_while_the_host_declares_legacy_stack - FAILED".into()];
        assert_eq!(
            judged_red_checks(
                &train(O280_ID, "ready"),
                "green",
                Some(&Standing::Failed),
                Some(&green),
                &fails,
            ),
            Some(vec!["test".to_string()])
        );
    }

    #[test]
    fn a_red_that_names_nothing_is_not_judged() {
        let unnamed = json!([{"conclusion": "FAILURE"}]);
        // An unnamed forge failure reads "unnamed check" in the rollup
        // reader; that is not a name.
        assert_eq!(
            judged_red_checks(
                &train(O280_ID, "ready"),
                "failing",
                Some(&Standing::Green),
                Some(&unnamed),
                &[],
            ),
            None
        );
        assert_eq!(
            judged_red_checks(
                &train(O280_ID, "ready"),
                "green",
                Some(&Standing::Failed),
                Some(&json!([{"context": "CI / web", "conclusion": "SUCCESS"}])),
                &[],
            ),
            None,
            "a red gate whose receipt names no check is not named"
        );
    }

    #[test]
    fn an_infrastructure_refusal_is_never_judged() {
        let refused = json!([
            {"context": "CI / locomotive", "conclusion": "FAILURE", "description": "refused: disk floor"}
        ]);
        assert_eq!(
            judged_red_checks(
                &train(O280_ID, "ready"),
                "failing",
                Some(&Standing::Failed),
                Some(&refused),
                &f7bd_gate_fails(),
            ),
            None
        );
    }

    #[test]
    fn a_merged_train_is_never_judged_whatever_its_checks_say() {
        let rollup = f7bd_rollup();
        assert_eq!(
            judged_red_checks(
                &train(F7BD_ID, "completed"),
                "failing",
                Some(&Standing::Failed),
                Some(&rollup),
                &f7bd_gate_fails(),
            ),
            None
        );
    }

    #[test]
    fn two_greens_are_not_a_red() {
        assert_eq!(
            judged_red_checks(
                &train(F7BD_ID, "ready"),
                "green",
                Some(&Standing::Green),
                Some(&json!([{"context": "CI / web", "conclusion": "SUCCESS"}])),
                &[],
            ),
            None
        );
    }

    #[test]
    fn a_gate_fails_line_names_the_check_before_its_colon() {
        assert_eq!(
            gate_check_names(&[
                "svelte-check: no cargo test failure in this check's output; 4 error line(s)"
                    .into(),
                "svelte-check: | error: script \"typecheck\" exited with code 1".into(),
                "test: a_test - panicked at crates/x/src/lib.rs:1:2: boom".into(),
                "no colon at all".into(),
            ]),
            vec!["svelte-check".to_string(), "test".to_string()]
        );
    }

    // -- locating the failure -----------------------------------------------

    #[test]
    fn the_gate_excerpt_locates_errors_and_not_warnings() {
        let (_, text) = &f7bd_gate_excerpt()[0];
        assert_eq!(
            located_failures(text),
            vec!["/gate-target/repo/apps/web/src/it/yard/phone-strip.test.ts".to_string()],
            "two located errors in one file, and Tabs.svelte's WARNING is not a failure"
        );
    }

    #[test]
    fn the_forge_log_tail_locates_through_colour_and_timestamps() {
        let rollup = f7bd_rollup();
        let tail = rollup[2]["log_tail"].as_str().unwrap();
        assert_eq!(
            located_failures(tail),
            vec!["/workspace/david/boss/apps/web/src/it/yard/phone-strip.test.ts".to_string()]
        );
    }

    #[test]
    fn a_panic_and_a_rustc_error_are_located_and_a_warning_is_not() {
        let text = "test: a_test - panicked at crates/core/boss-jobs/src/car.rs:12:5:\n\
                    warning: unused import\n  --> crates/a/src/warn.rs:1:5\n\
                    error[E0308]: mismatched types\n  --> crates/b/src/lib.rs:40:9\n";
        assert_eq!(
            located_failures(text),
            vec![
                "crates/b/src/lib.rs".to_string(),
                "crates/core/boss-jobs/src/car.rs".to_string(),
            ]
        );
    }

    // -- a whole-tree scanner names the car's file in its message ---------
    //
    // Train 22789ff1 (2026-09-26 04:47): the train gate (gate-run
    // 94ac9e33) failed boss-testing's the_sim_bypass_has_one_door, a
    // tree-wide pin no car changed, and its panic MESSAGE named the two
    // files car 9acb27f1 had added — as `path:line`, no column. Only the
    // panic's own location was read, so the red read "outside the
    // consist", every car was released unstruck, and the next train
    // (7f3bc955) boarded the same car again (backlog 4a56c797). The
    // receipt's `fails` line and `fails_excerpt`, trimmed, never retyped.

    const T22789_FAILS: &str = "test: the_bypass_is_built_only_from_the_deployments_switch - panicked at crates/core/boss-testing/tests/the_sim_bypass_has_one_door.rs:147:5: the sim bypass is installed here without the deployment's switch \u{2014} build it with SimBypassPolicyClient::from_env(inner), which returns the inner client unless BOSS_SIM_ENABLED is on (backlog 85e7f10f): crates/modules/boss-assets/tests/the_assets_api_wires_po... (+81 char(s); the Job log replay has the full text)";

    const T22789_EXCERPT: &str = "... (245 line(s) before the first failure marker omitted; the Job log replay has them)\n\
     Running tests/the_sim_bypass_has_one_door.rs (/gate-target/target/debug/deps/the_sim_bypass_has_one_door-3f0bcbfc763579a0)\n\
\n\
running 3 tests\n\
test the_chain_flag_is_read_for_provenance_and_never_to_authorize ... ok\n\
test the_scanners_read_code_and_not_prose ... ok\n\
test the_bypass_is_built_only_from_the_deployments_switch ... FAILED\n\
\n\
failures:\n\
\n\
---- the_bypass_is_built_only_from_the_deployments_switch stdout ----\n\
\n\
thread 'the_bypass_is_built_only_from_the_deployments_switch' (301414) panicked at crates/core/boss-testing/tests/the_sim_bypass_has_one_door.rs:147:5:\n\
the sim bypass is installed here without the deployment's switch \u{2014} build it with SimBypassPolicyClient::from_env(inner), which returns the inner client unless BOSS_SIM_ENABLED is on (backlog 85e7f10f):\n\
  crates/modules/boss-assets/tests/the_assets_api_wires_policy.rs:30\n\
  crates/modules/boss-commerce/tests/the_commerce_api_wires_policy.rs:30\n\
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\
\n\
\n\
failures:\n\
    the_bypass_is_built_only_from_the_deployments_switch\n\
\n\
test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.27s\n\
\n\
error: test failed, to rerun pass `-p boss-testing --test the_sim_bypass_has_one_door`";

    const SIM_DOOR: &str = "crates/core/boss-testing/tests/the_sim_bypass_has_one_door.rs";
    const ASSETS_PIN: &str = "crates/modules/boss-assets/tests/the_assets_api_wires_policy.rs";
    const COMMERCE_PIN: &str =
        "crates/modules/boss-commerce/tests/the_commerce_api_wires_policy.rs";

    #[test]
    fn train_22789ff1_locates_the_files_its_panic_message_names() {
        assert_eq!(
            located_failures(T22789_EXCERPT),
            vec![
                SIM_DOOR.to_string(),
                ASSETS_PIN.to_string(),
                COMMERCE_PIN.to_string(),
            ],
            "the panic's location AND the path:line offenders its message lists"
        );
        // The receipt's one-line rung truncates the message before the
        // first offender's line number: it locates the panic alone.
        assert_eq!(located_failures(T22789_FAILS), vec![SIM_DOOR.to_string()]);
    }

    #[test]
    fn train_22789ff1_holds_the_car_its_message_names() {
        let located = verdict_located_files(
            &[T22789_FAILS.to_string()],
            &[("test".to_string(), T22789_EXCERPT.to_string())],
            None,
        );
        // Car 9acb27f1's files are the two the packet names it as adding;
        // the second car is 19bee523 of the same consist, its file list
        // a stand-in (its branch is gone): it changed neither pin.
        let cars = vec![
            (
                "19bee523-9904-43c2-91ea-c6778bc07c13".to_string(),
                vec!["crates/orchestrators/boss-cli/src/train/dock.rs".to_string()],
            ),
            (
                "9acb27f1-00d0-474c-b5ac-5862b5b1437a".to_string(),
                vec![ASSETS_PIN.to_string(), COMMERCE_PIN.to_string()],
            ),
        ];
        assert_eq!(
            cars_to_hold(&located, &cars),
            vec![(
                "9acb27f1-00d0-474c-b5ac-5862b5b1437a".to_string(),
                vec![ASSETS_PIN.to_string(), COMMERCE_PIN.to_string()]
            )]
        );
    }

    #[test]
    fn a_message_path_counts_only_inside_a_panic_and_only_with_a_line() {
        let text = "  crates/a/src/before.rs:3\n\
                    thread 't' panicked at crates/x/tests/pin.rs:9:5:\n\
                    see crates/a/src/prose.rs and docs/design/x.md, not a location;\n\
                    offender (crates/b/src/lib.rs:12), also crates/c/src/lib.rs:4:2.\n\
                    note: run with `RUST_BACKTRACE=1`\n\
                    crates/d/src/after.rs:7\n";
        assert_eq!(
            located_failures(text),
            vec![
                "crates/b/src/lib.rs".to_string(),
                "crates/c/src/lib.rs".to_string(),
                "crates/x/tests/pin.rs".to_string(),
            ]
        );
    }

    #[test]
    fn the_verdict_locates_across_both_halves() {
        let rollup = f7bd_rollup();
        assert_eq!(
            verdict_located_files(&f7bd_gate_fails(), &f7bd_gate_excerpt(), Some(&rollup)),
            vec![
                "/gate-target/repo/apps/web/src/it/yard/phone-strip.test.ts".to_string(),
                "/workspace/david/boss/apps/web/src/it/yard/phone-strip.test.ts".to_string(),
            ]
        );
    }

    // -- holding the car the failure names ----------------------------------

    #[test]
    fn train_f7bd1e9d_holds_car_g_alone() {
        let rollup = f7bd_rollup();
        let located =
            verdict_located_files(&f7bd_gate_fails(), &f7bd_gate_excerpt(), Some(&rollup));
        assert_eq!(
            cars_to_hold(&located, &f7bd_car_files()),
            vec![(
                "6c7c75f7-4f34-4f5f-87c0-3737e92a66ed".to_string(),
                vec!["apps/web/src/it/yard/phone-strip.test.ts".to_string()]
            )],
            "the failing file is car G's alone; the shed car is released, not held"
        );
    }

    #[test]
    fn a_file_two_cars_changed_holds_neither() {
        let located =
            vec!["/gate-target/repo/apps/web/src/it/yard/phone-strip.test.ts".to_string()];
        let both = vec![
            (
                "car-a".to_string(),
                vec!["apps/web/src/it/yard/phone-strip.test.ts".to_string()],
            ),
            (
                "car-b".to_string(),
                vec!["apps/web/src/it/yard/phone-strip.test.ts".to_string()],
            ),
        ];
        assert!(cars_to_hold(&located, &both).is_empty());
    }

    #[test]
    fn a_file_no_car_changed_holds_nobody() {
        let located = vec!["/gate-target/repo/libs/web-kit/src/ui/Tabs.svelte".to_string()];
        assert!(cars_to_hold(&located, &f7bd_car_files()).is_empty());
    }

    #[test]
    fn a_top_level_file_matches_only_itself_never_a_same_named_file_deeper() {
        let located = vec!["/gate-target/repo/crates/x/Cargo.toml".to_string()];
        let cars = vec![("car-a".to_string(), vec!["Cargo.toml".to_string()])];
        assert!(
            cars_to_hold(&located, &cars).is_empty(),
            "the root Cargo.toml is not crates/x/Cargo.toml"
        );
        let exact = vec!["Cargo.toml".to_string()];
        assert_eq!(cars_to_hold(&exact, &cars).len(), 1);
    }

    #[test]
    fn two_files_each_one_cars_hold_both_cars() {
        let located = vec![
            "crates/a/src/lib.rs".to_string(),
            "/gate-target/repo/apps/web/src/b.ts".to_string(),
        ];
        let cars = vec![
            ("car-a".to_string(), vec!["crates/a/src/lib.rs".to_string()]),
            ("car-b".to_string(), vec!["apps/web/src/b.ts".to_string()]),
        ];
        assert_eq!(
            cars_to_hold(&located, &cars),
            vec![
                ("car-a".to_string(), vec!["crates/a/src/lib.rs".to_string()]),
                ("car-b".to_string(), vec!["apps/web/src/b.ts".to_string()]),
            ]
        );
    }

    #[test]
    fn the_hold_names_the_train_the_check_the_file_and_the_way_out() {
        let r = judged_red_hold_reason(
            F7BD_ID,
            &["CI / web (pull_request)".into(), "svelte-check".into()],
            &["apps/web/src/it/yard/phone-strip.test.ts".into()],
            None,
        );
        for want in [
            "f7bd1e9d",
            "CI / web (pull_request)",
            "apps/web/src/it/yard/phone-strip.test.ts",
            "boss release",
        ] {
            assert!(r.contains(want), "hold reason lacks {want:?}: {r}");
        }
    }

    // -- a forge red whose culprit the conductor can name (b4a95f47) -------
    //
    // Train 496122a0 (2026-10-18 17:53Z): ONE car aboard, 10cb67a3
    // (feat/tag-the-ci-runner-image-by-its-content-stamp-2), red on a
    // named `CI / web` while its train gate was never filed — the cluster
    // was at its gate bound, and a train judged on CI alone launches no
    // gate after — so `judged_red_checks` waited for a half that would
    // never speak and the train held the one track ~95 minutes, five
    // released urgent cars behind it, until an operator cancelled it by
    // hand. The rollup entry is the record's own: its log tail as the
    // train's `ci` step stamped it, trimmed, never retyped. The failing
    // test reads ci.yml, which the car changed; the log locates the
    // test, which no car changed — so the file rule alone names nobody.

    const T4961_ID: &str = "496122a0-1aa0-4188-8946-4cebb83f6122";
    const T4961_CAR: &str = "10cb67a3-70fa-4a6a-96e3-c2dd13c00a65";

    fn t4961_rollup() -> Value {
        json!([
            {"context": "CI / delivery-plan (pull_request)", "conclusion": "SUCCESS", "description": ""},
            {"context": "CI / build-image (pull_request)", "conclusion": "SUCCESS", "description": ""},
            {"context": "CI / locomotive (pull_request)", "conclusion": "SUCCESS", "description": ""},
            {"context": "CI / web (pull_request)", "conclusion": "FAILURE", "description": "",
             "log_tail": "…(the failure, with context, 8 more further down)\n\
        2026-10-18T17:53:00.1733544Z error: expect(received).toEqual(expected)\n\
        2026-10-18T17:53:00.1733620Z \n\
        2026-10-18T17:53:00.1733701Z - []\n\
        2026-10-18T17:53:00.1733772Z + [\n\
        2026-10-18T17:53:00.1733859Z +   \"forge web job is absent, conditional, failure-tolerant or unreadable\",\n\
        2026-10-18T17:53:00.1733949Z + ]\n\
        2026-10-18T17:53:00.1734023Z \n\
        2026-10-18T17:53:00.1734322Z       at <anonymous> (/workspace/david/boss/apps/web/src/shell/gate-matches-ci.test.ts:58:34)\n\
        2026-10-18T17:53:00.1735184Z (fail) web coverage across both required train lanes > current and assembled residual forge shapes cover the local gate [0.44ms]"}
        ])
    }

    /// What car 10cb67a3 changed, off the train's delivery plan.
    fn t4961_car_files() -> Vec<(String, Vec<String>)> {
        vec![(
            T4961_CAR.to_string(),
            vec![
                ".forgejo/workflows/ci.yml".to_string(),
                "crates/core/boss-testing/tests/lighter_ci.rs".to_string(),
                "crates/core/boss-testing/tests/the_ci_image_is_named_by_its_recipe.rs".to_string(),
                "docs/architecture-decisions.md".to_string(),
                "infra/forge/prune-ci-images.lib.sh".to_string(),
                "infra/lint/ci-tools-declared.sh".to_string(),
            ],
        )]
    }

    #[test]
    fn train_496122a0_forge_red_with_no_gate_is_judged_on_the_forge_alone() {
        let rollup = t4961_rollup();
        let train = train(T4961_ID, "ready");
        assert_eq!(
            judged_red_checks(&train, "failing", None, Some(&rollup), &[]),
            None,
            "control: the both-halves rule waits for a gate that was never filed"
        );
        for gate in [
            None,
            Some(Standing::Pending),
            Some(Standing::Lost),
            Some(Standing::Refused("disk floor".into())),
            Some(Standing::Unavailable("no kubectl".into())),
        ] {
            assert_eq!(
                forge_judged_red_checks(&train, "failing", gate.as_ref(), Some(&rollup)),
                Some(vec!["CI / web (pull_request)".to_string()]),
                "gate {gate:?}: the forge has spoken red and named its check"
            );
        }
    }

    #[test]
    fn the_forge_alone_judges_only_a_named_unrefused_unmerged_red_the_gate_has_not_judged() {
        let rollup = t4961_rollup();
        let open = train(T4961_ID, "ready");
        // A finished gate is the both-halves rule's to judge.
        for gate in [Standing::Failed, Standing::Green] {
            assert_eq!(
                forge_judged_red_checks(&open, "failing", Some(&gate), Some(&rollup)),
                None,
                "{gate:?}"
            );
        }
        for verdict in ["green", "pending", "aborted"] {
            assert_eq!(
                forge_judged_red_checks(&open, verdict, None, Some(&rollup)),
                None,
                "{verdict}"
            );
        }
        assert_eq!(
            forge_judged_red_checks(
                &train(T4961_ID, "completed"),
                "failing",
                None,
                Some(&rollup)
            ),
            None,
            "a merged train is never judged"
        );
        let refused = json!([
            {"context": "CI / locomotive", "conclusion": "FAILURE", "description": "refused: disk floor"}
        ]);
        assert_eq!(
            forge_judged_red_checks(&open, "failing", None, Some(&refused)),
            None,
            "infrastructure is not a verdict"
        );
        let unnamed = json!([{"conclusion": "FAILURE"}]);
        assert_eq!(
            forge_judged_red_checks(&open, "failing", None, Some(&unnamed)),
            None,
            "a red that names nothing is not judged"
        );
    }

    #[test]
    fn train_496122a0_names_its_only_car_as_the_culprit() {
        let rollup = t4961_rollup();
        let located = verdict_located_files(&[], &[], Some(&rollup));
        assert!(
            cars_to_hold(&located, &t4961_car_files()).is_empty(),
            "control: the log locates no file the car changed: {located:?}"
        );
        let culprits = culprit_cars(&located, &t4961_car_files());
        assert_eq!(culprits, vec![(T4961_CAR.to_string(), Vec::new())]);
        let reason =
            forge_culprit_cancel_reason(&["CI / web (pull_request)".to_string()], &culprits, 6)
                .expect("a named culprit cancels");
        for want in [
            "CI / web (pull_request)",
            "10cb67a3",
            "only car aboard",
            "b4a95f47",
            "6h",
        ] {
            assert!(
                reason.contains(want),
                "cancel reason lacks {want:?}: {reason}"
            );
        }
        // Review 38f094c5: the reason is written BEFORE the best-effort
        // hold runs, and on whichever pass names the culprit — so it may
        // claim neither a hold that has landed nor the first reconcile.
        for never in ["culprit is held", "first reconcile"] {
            assert!(
                !reason.contains(never),
                "cancel reason claims {never:?}: {reason}"
            );
        }
    }

    /// The other half of the acceptance: a multi-car forge red that no
    /// file pins on one car names no culprit, so nothing is cancelled
    /// early — it pages as today and the stall rule stays its backstop.
    #[test]
    fn a_multi_car_forge_red_with_no_culprit_cancels_nothing_early() {
        let rollup = t4961_rollup();
        let located = verdict_located_files(&[], &[], Some(&rollup));
        let mut two = t4961_car_files();
        two.push((
            "eed1f6a0-a7ea-4884-9819-a1fe2d6b009b".to_string(),
            vec!["infra/cluster/dev-memory-watch.sh".to_string()],
        ));
        let culprits = culprit_cars(&located, &two);
        assert!(culprits.is_empty(), "{culprits:?}");
        assert_eq!(
            forge_culprit_cancel_reason(&["CI / web (pull_request)".to_string()], &culprits, 6),
            None
        );
        assert!(
            culprit_cars(&located, &[]).is_empty(),
            "no car read is no culprit"
        );
    }

    /// A multi-car forge red whose log locates a file only one car
    /// changed names THAT car, as the gate's located rule always has.
    #[test]
    fn a_multi_car_forge_red_located_in_one_cars_file_names_that_car() {
        let rollup = f7bd_rollup();
        let located = verdict_located_files(&[], &[], Some(&rollup));
        let culprits = culprit_cars(&located, &f7bd_car_files());
        assert_eq!(
            culprits,
            vec![(
                "6c7c75f7-4f34-4f5f-87c0-3737e92a66ed".to_string(),
                vec!["apps/web/src/it/yard/phone-strip.test.ts".to_string()]
            )]
        );
        let reason =
            forge_culprit_cancel_reason(&["CI / web (pull_request)".to_string()], &culprits, 6)
                .expect("named");
        assert!(
            reason.contains("6c7c75f7") && reason.contains("phone-strip.test.ts"),
            "{reason}"
        );
    }

    #[test]
    fn the_failure_line_is_the_first_failing_line_of_the_failing_check() {
        assert_eq!(
            first_failure_line(Some(&t4961_rollup()), &[]).as_deref(),
            Some("CI / web (pull_request): error: expect(received).toEqual(expected)")
        );
        assert_eq!(
            first_failure_line(Some(&f7bd_rollup()), &[]).as_deref(),
            Some(
                "CI / web (pull_request): Error: Conversion of type '{ thirds: { third: string; regions: string[]; }[]; }' may be a mistake"
            )
        );
        // No forge log: the gate receipt's first `fails` line is the line.
        assert_eq!(
            first_failure_line(None, &f7bd_gate_fails()).as_deref(),
            Some(f7bd_gate_fails()[0].as_str())
        );
        assert_eq!(first_failure_line(None, &[]), None);
        let long = vec![format!("test: {}", "x".repeat(2000))];
        let line = first_failure_line(None, &long).expect("a line");
        assert!(
            line.len() <= FAILURE_LINE_BYTES + '…'.len_utf8(),
            "{}",
            line.len()
        );
    }

    #[test]
    fn a_sole_cars_hold_names_the_check_and_the_failing_line() {
        let line = first_failure_line(Some(&t4961_rollup()), &[]);
        let r = judged_red_hold_reason(
            T4961_ID,
            &["CI / web (pull_request)".into()],
            &[],
            line.as_deref(),
        );
        for want in [
            "496122a0",
            "CI / web (pull_request)",
            "only car aboard",
            "error: expect(received).toEqual(expected)",
            "boss release",
        ] {
            assert!(r.contains(want), "hold reason lacks {want:?}: {r}");
        }
    }
}
