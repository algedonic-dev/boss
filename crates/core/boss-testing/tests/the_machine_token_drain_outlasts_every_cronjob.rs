//! The machine token's drain window outlasts every CronJob (design
//! 6805c764, car 3; backlog 2710c8fc).
//!
//! A rotation keeps the old token accepted as `previous` until every
//! gate's tally has shown, for `drain_minutes`, that nothing presents it
//! — then blanks it. A CronJob that runs once a day and still holds the
//! old value (its pod started before the promotion reached its mount)
//! presents it only when it next runs. A window shorter than the longest
//! CronJob period could close between two of its runs, and that caller
//! would be refused on its next one. The design fixed the window at "the
//! longest CronJob period in the manifests, with a floor of one hour".
//!
//! The period is a fact of infra/cluster/manifests/; the window is an arg
//! of the rule row. A fact that lives twice (CLAUDE.md §9a), pinned here:
//! a CronJob added with a weekly schedule fails this test, naming its
//! file, until the window grows to match.

use boss_testing::repo_root;

const RULE: &str = "infra/dispatcher/rules/broker-rotates-the-machine-token.toml";
const MANIFESTS: &str = "infra/cluster/manifests";
/// `DRAIN_FLOOR_MINUTES` in the handler (credential_rotate_self_issued.rs).
const FLOOR: u64 = 60;

/// The longest gap between two runs of a five-field cron schedule, in
/// minutes. Only the shapes the manifests use are read; anything else
/// is refused by name, so a new shape is a decision here, not a guess.
fn period_minutes(schedule: &str) -> Result<u64, String> {
    let f: Vec<&str> = schedule.split_whitespace().collect();
    let [minute, hour, dom, month, dow] = f[..] else {
        return Err(format!("{schedule:?} is not five fields"));
    };
    let literal = |s: &str| s.split(',').all(|p| p.parse::<u32>().is_ok());
    let step = |s: &str| s.strip_prefix("*/").and_then(|n| n.parse::<u64>().ok());
    if dom != "*" || month != "*" {
        return Ok(31 * 24 * 60);
    }
    if dow != "*" {
        return Ok(7 * 24 * 60);
    }
    match (minute, hour) {
        (m, h) if literal(m) && literal(h) => Ok(24 * 60),
        (m, "*") if literal(m) => Ok(60),
        (m, h) if literal(m) && step(h).is_some() => Ok(60 * step(h).unwrap_or(1)),
        (m, "*") if step(m).is_some() => Ok(step(m).unwrap_or(1)),
        ("*", "*") => Ok(1),
        _ => Err(format!(
            "{schedule:?}: a shape this pin does not read — teach period_minutes it"
        )),
    }
}

fn schedules() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(repo_root().join(MANIFESTS)).expect("the manifests") {
        let path = entry.expect("an entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a manifest");
        for line in text.lines() {
            let Some(rest) = line.trim_start().strip_prefix("schedule:") else {
                continue;
            };
            let value = rest.split('#').next().unwrap_or_default().trim();
            out.push((
                path.file_name().unwrap().to_string_lossy().into_owned(),
                value.trim_matches(|c| c == '"' || c == '\'').to_string(),
            ));
        }
    }
    out
}

fn drain_minutes() -> u64 {
    let text = std::fs::read_to_string(repo_root().join(RULE)).expect("the rotation rule");
    let doc: toml::Value = toml::from_str(&text).expect("the rule parses");
    let raw = doc["rule"][0]["do"][0]["args"]["drain_minutes"]
        .as_str()
        .expect("drain_minutes is an expr string arg");
    raw.trim_matches('"')
        .parse()
        .expect("drain_minutes is a whole number of minutes")
}

#[test]
fn the_drain_window_is_at_least_the_longest_cronjob_period_and_the_floor() {
    let schedules = schedules();
    assert!(
        schedules.len() >= 5,
        "the manifests declare CronJobs; reading none means this pin reads the wrong place: \
         {schedules:?}"
    );
    let (file, schedule, longest) = schedules
        .iter()
        .map(|(f, s)| {
            let p = period_minutes(s).unwrap_or_else(|e| panic!("{f}: {e}"));
            (f.clone(), s.clone(), p)
        })
        .max_by_key(|(_, _, p)| *p)
        .expect("at least one schedule");
    let window = drain_minutes();
    assert!(
        window >= FLOOR,
        "drain_minutes {window} is under the {FLOOR}-minute floor"
    );
    assert!(
        window >= longest,
        "drain_minutes is {window}, but {file} runs on {schedule:?} — every {longest} minutes. \
         A caller that holds the old token and runs that rarely would present it after the \
         revoke: raise drain_minutes in {RULE} (and its advance twin) to at least {longest}"
    );
}

#[test]
fn the_period_reader_knows_the_shapes_it_reads() {
    for (schedule, want) in [
        ("*/5 * * * *", 5),
        ("20 * * * *", 60),
        ("15 4 * * *", 1440),
        ("0 */6 * * *", 360),
        ("0 3 * * 0", 10080),
        ("0 3 1 * *", 44640),
    ] {
        assert_eq!(period_minutes(schedule), Ok(want), "{schedule}");
    }
    assert!(period_minutes("@daily").is_err());
}
