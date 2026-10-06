//! `boss automations unregistered [--hours N]` — name every
//! `automation:*` id that wrote to the audit log over the window and
//! holds no row in the agents registry. Report only: it exits 0
//! whatever it finds and writes nothing.
//!
//! WHY (backlog ddf0773e, design abf9eeae car 1, 2026-10-01). The
//! design moves a machine caller's role off its own header and onto a
//! registry row, and a registry read has no answer for an id with no
//! row — at enforce (car 4) such an id gets the read-only floor. So a
//! NEW automation that starts writing without a row must be named
//! while that costs nothing: this verb is the measurement car 1 was
//! built from, made repeatable, and its naming is
//! `boss_jobs::agents::automations::unregistered`, the one function the
//! unit tests hold. Car 2's resolver tallies the same thing per request
//! once it runs in report mode; this reads the record that already
//! exists.
//!
//! WHAT IT READS. The registry through `GET /api/agents/automations`
//! (a family member — `automation:rule:<name>` — is answered by its
//! signer's `signs_for`), and the audit log through `GET
//! /api/events/export` in one-hour pages. A page that comes back at the
//! export's 50,000-row cap is REFUSED, never counted: a capped page is
//! a smaller question answered, and a writer in its unread tail would
//! read as absent (a limit is not a filter, e7cf78c6).

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use boss_jobs::agents::AutomationActor;
use boss_jobs::agents::automations::{AUTOMATION_PREFIX, row_of_record, unregistered};
use chrono::{DateTime, Duration, Utc};

/// The export's own cap (`boss_events::tail_http`, `limit` clamped to
/// 50,000): a page holding this many rows may have been cut.
pub(crate) const EXPORT_CAP: usize = 50_000;

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// The automation half of the agents registry — which automation ids write, and which hold no row.
    Automations {
        #[command(subcommand)]
        action: Action,
    },
}

#[derive(clap::Subcommand)]
pub enum Action {
    /// Name every automation id that wrote to the audit log over the window with no row in the registry. Report only: exits 0 whatever it finds.
    Unregistered {
        /// The window, in hours back from now, read in one-hour pages.
        #[arg(long, default_value_t = 24)]
        hours: u32,
    },
}

/// The one-hour pages covering `[end - hours, end)`, oldest first.
pub(crate) fn pages(end: DateTime<Utc>, hours: u32) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    (0..i64::from(hours))
        .rev()
        .map(|back| {
            let until = end - Duration::hours(back);
            (until - Duration::hours(1), until)
        })
        .collect()
}

/// Add one export page's writers to `into`: the `_actor` of every
/// JSONL row that carries one. A line that does not parse is an error
/// naming its number — a tally that skipped it would undercount.
pub(crate) fn tally(page: &str, into: &mut BTreeMap<String, u64>) -> Result<usize> {
    let mut rows = 0;
    for (n, line) in page
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        rows += 1;
        let v: serde_json::Value =
            serde_json::from_str(line).with_context(|| format!("export line {}", n + 1))?;
        if let Some(actor) = v["payload"]["_actor"].as_str() {
            *into.entry(actor.to_string()).or_default() += 1;
        }
    }
    Ok(rows)
}

/// The report: every automation writer with the row that answers for
/// it, then the unregistered ones, then the one-line verdict.
pub(crate) fn render(
    rows: &[AutomationActor],
    writers: &BTreeMap<String, u64>,
    window: &str,
) -> String {
    let mut out = format!("automation writers, {window}:\n");
    let mut registered: Vec<(&str, u64, &AutomationActor)> = writers
        .iter()
        .filter(|(id, _)| id.starts_with(AUTOMATION_PREFIX))
        .filter_map(|(id, n)| row_of_record(rows, id).map(|r| (id.as_str(), *n, r)))
        .collect();
    registered.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    for (id, n, row) in &registered {
        let via = if row.id == *id {
            String::new()
        } else {
            format!(" (via {})", row.id)
        };
        out.push_str(&format!("  {n:>8}  {id}  {}{via}\n", row.role));
    }
    let missing = unregistered(rows, writers);
    for (id, n) in &missing {
        out.push_str(&format!("  {n:>8}  {id}  UNREGISTERED\n"));
    }
    out.push_str(&format!(
        "unregistered: {} of {} automation writers{}\n",
        missing.len(),
        registered.len() + missing.len(),
        if missing.is_empty() {
            String::new()
        } else {
            " — add a row under infra/platform/automations/ naming the role each signs as"
                .to_string()
        }
    ));
    out
}

/// `end` is the window's close — the wall clock, read once in `main`
/// (the one boss-cli file no-wallclock admits), so the report pages
/// back from a single instant.
pub async fn dispatch(cmd: Cmd, end: DateTime<Utc>) -> Result<()> {
    let Cmd::Automations {
        action: Action::Unregistered { hours },
    } = cmd;
    if hours == 0 {
        bail!("--hours 0 reads no window; name at least one hour");
    }
    let jobs = crate::gate::resolve_jobs_base(None)?;
    let jobs = jobs.trim_end_matches('/').to_string();
    let events =
        crate::events::events_base_from(std::env::var(crate::events::EVENTS_ENV).ok(), &jobs);
    let client = crate::gate::machine_client()?;
    let user = crate::identity::header(&crate::identity::reader());

    let url = format!("{jobs}/api/agents/automations");
    let resp = crate::train::send_through_a_roll(&format!("GET {url}"), || {
        client.get(&url).header("x-boss-user", user.as_str())
    })
    .await?;
    if !resp.status().is_success() {
        bail!("GET {url} returned {}", resp.status());
    }
    #[derive(serde::Deserialize)]
    struct Listing {
        data: Vec<AutomationActor>,
    }
    let rows = resp
        .json::<Listing>()
        .await
        .with_context(|| format!("decoding {url}"))?
        .data;

    let mut writers = BTreeMap::new();
    for (since, until) in pages(end, hours) {
        let url = format!(
            "{events}/api/events/export?since={}&until={}",
            since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            until.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        let resp = client
            .get(&url)
            .header("x-boss-user", user.as_str())
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            bail!("GET {url} returned {}", resp.status());
        }
        let page = resp
            .text()
            .await
            .with_context(|| format!("reading {url}"))?;
        let n = tally(&page, &mut writers)?;
        if n >= EXPORT_CAP {
            bail!(
                "GET {url} returned {n} rows, the export's cap: the page may be cut, and a \
                 writer in its unread tail would read as absent. Refusing to report on it."
            );
        }
    }
    let window = format!(
        "{} to {} ({hours}h)",
        (end - Duration::hours(i64::from(hours))).format("%Y-%m-%dT%H:%MZ"),
        end.format("%Y-%m-%dT%H:%MZ")
    );
    print!("{}", render(&rows, &writers, &window));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, signs_for: Option<&str>) -> AutomationActor {
        AutomationActor {
            id: id.into(),
            role: "platform-admin".into(),
            description: "x".into(),
            signs_for: signs_for.map(Into::into),
        }
    }

    #[test]
    fn the_pages_cover_the_window_oldest_first_an_hour_each() {
        let end: DateTime<Utc> = "2026-10-01T14:00:00Z".parse().unwrap();
        let p = pages(end, 3);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].0.to_rfc3339(), "2026-10-01T11:00:00+00:00");
        assert_eq!(p[2].1, end);
        assert!(p.windows(2).all(|w| w[0].1 == w[1].0), "contiguous");
    }

    #[test]
    fn a_page_tallies_every_actor_and_refuses_a_line_it_cannot_read() {
        let mut w = BTreeMap::new();
        let page = concat!(
            r#"{"payload":{"_actor":"automation:gate-runner"}}"#,
            "\n",
            r#"{"payload":{"_actor":"automation:gate-runner"}}"#,
            "\n\n",
            r#"{"payload":{}}"#,
            "\n"
        );
        assert_eq!(tally(page, &mut w).unwrap(), 3);
        assert_eq!(w["automation:gate-runner"], 2);
        assert!(tally("{not json\n", &mut w).is_err());
    }

    /// The verb's whole point, at its output: a new automation that
    /// writes without a row is NAMED with its count, a family member is
    /// shown through its signer, and the verdict counts both.
    #[test]
    fn the_report_names_the_unregistered_writer() {
        let rows = vec![
            row("automation:gate-runner", None),
            row("automation:dispatcher", Some("automation:rule:")),
        ];
        let writers: BTreeMap<String, u64> = [
            ("automation:gate-runner", 340),
            ("automation:rule:converge-on-merge", 48),
            ("automation:a-new-sweep", 5),
            ("agent-claude", 2861),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let out = render(&rows, &writers, "a window");
        assert!(
            out.contains("automation:a-new-sweep  UNREGISTERED"),
            "{out}"
        );
        assert!(
            out.contains(
                "automation:rule:converge-on-merge  platform-admin (via automation:dispatcher)"
            ),
            "{out}"
        );
        assert!(
            out.contains("unregistered: 1 of 3 automation writers —"),
            "{out}"
        );
        assert!(!out.contains("agent-claude"), "{out}");
        let clean = render(&rows, &BTreeMap::new(), "a window");
        assert!(
            clean.ends_with("unregistered: 0 of 0 automation writers\n"),
            "{clean}"
        );
    }
}
