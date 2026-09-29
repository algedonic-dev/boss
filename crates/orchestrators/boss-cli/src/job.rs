//! `boss job` — read, file, and patch packets without hand-building
//! the HTTP that talks about them.
//!
//! WHY THIS IS A VERB. Counted on 2026-08-30, one session: ~23
//! hand-typed `curl` invocations against the jobs API, each carrying
//! the `x-boss-user` header inline (514b39d8). Every one of those is a
//! chance at the two failure classes this crate keeps re-learning:
//!
//! - THE WRONG ACTOR READS AS AN EMPTY SYSTEM. A misspelled role or a
//!   missing header does not error — the API deliberately returns an
//!   empty collection, which once read as catastrophic data loss.
//!   A verb carries the one correct identity; a fresh curl carries
//!   whatever was typed.
//! - THE 422 DANCE. `POST /api/jobs` reports ONE missing envelope
//!   field per 422 (f5dd5167), so filing a packet by hand is three
//!   round-trips of guessing. `file` defaults the whole envelope.
//!
//! CONFIRMATION OVER STATUS CODES, everywhere. This same session hit
//! two silent 204 no-ops — a step PUT carrying an unknown field name,
//! and the write-once class (a07cfddd) — so no action here reports
//! success from a status code: `file` re-reads the packet it created,
//! and `patch` re-reads and FAILS unless every key it sent is now
//! actually on the packet.
//!
//! DELIBERATELY EXCLUDED: step completion (three verbs already own
//! their steps, and a generic step-writer would grade its own
//! homework), and the full-body job PUT — a read-modify-write replace
//! that has corrupted the system of record before. Not wrapped;
//! wrapping it would make it convenient.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

/// A full job id as the API expects it: 36 chars, dashed. Anything
/// else goes through list-and-match resolution.
pub(crate) fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Resolve a job reference against fetched rows, in `boss job`'s own
/// vocabulary. The matching semantics live in `prove::matching_jobs`
/// — shared, not copied — but the refusals speak about jobs of any
/// kind, because that is what this verb sees.
pub(crate) fn resolve<'a>(rows: &'a [Value], given: &str) -> Result<&'a Value> {
    let matches = crate::prove::matching_jobs(rows, given);
    match matches.len() {
        1 => Ok(matches[0]),
        0 => {
            if given.len() < 8 && !given.contains('/') {
                bail!(
                    "{given:?} is too short to resolve — give at least 8 characters \
                     of the id, the full uuid, or the branch exactly"
                );
            }
            bail!("no job matches {given:?} in the fetched rows")
        }
        n => {
            // LIST THEM, never pick one. And read the id and the name
            // through `envelope`, so a row that spells them `job_id` /
            // `job_title` is listed rather than rendered as `?  ?` —
            // which is a refusal the reader cannot act on.
            let mut listed = String::new();
            for m in &matches {
                listed.push_str(&format!(
                    "\n  {}  {}",
                    crate::envelope::job_id(m).unwrap_or("?"),
                    crate::envelope::job_title(m).unwrap_or("?")
                ));
            }
            bail!("{n} jobs match {given:?} — say which:{listed}")
        }
    }
}

/// The envelope `POST /api/jobs` actually requires, learned one 422 at
/// a time (f5dd5167). Explicit values win; everything else lands.
///
/// No `opened_on`: the create handler injects it off the authoritative
/// (sim-aware) clock AND stamps the precise filing instant beside it as
/// `metadata.opened_at` — but only when the clock owns the date. This
/// envelope used to send the caller's `today`, which read as a
/// deliberate (backdated) date and silenced the stamp on every packet
/// `boss job file` / `boss ops` filed, so timing one meant reading its
/// event stream (backlog a7a07ffb, 2026-09-15).
pub(crate) fn envelope(
    kind: &str,
    title: &str,
    priority: Option<&str>,
    subject_id: Option<&str>,
    owner_id: &str,
    metadata: Option<Value>,
) -> Value {
    json!({
        "kind": kind,
        "title": title,
        "tags": [],
        "subject": {
            "id": subject_id.unwrap_or("bosspipeline"),
            "subject_kind": "custom",
        },
        "owner_id": owner_id,
        "status": "open",
        "priority": priority.unwrap_or("standard"),
        "metadata": metadata.unwrap_or_else(|| json!({})),
    })
}

/// One list line: 8-char id, kind, status, title — cut to `width` so a
/// narrow terminal shows one job per line instead of a wrapped mess.
pub(crate) fn list_line(row: &Value, width: usize) -> String {
    // Through `envelope`, so one definition of "where the id is" serves
    // every row shape this verb can be handed.
    let id = crate::envelope::job_id(row).unwrap_or("????????");
    let line = format!(
        "{}  {}  {}  {}",
        &id[..8.min(id.len())],
        row.get("kind").and_then(Value::as_str).unwrap_or("?"),
        row.get("status").and_then(Value::as_str).unwrap_or("?"),
        crate::envelope::job_title(row).unwrap_or("?")
    );
    fit(&line, width)
}

pub(crate) fn fit(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let cut: String = s.chars().take(width.saturating_sub(3)).collect();
    format!("{cut}...")
}

/// The packet, for a person, in a shape that is THIS VERB's contract
/// rather than the endpoint's envelope (backlog d44f6152).
///
/// It answers the three questions actually asked of a packet:
///
/// 1. WHAT KIND OF PACKET IS THIS — kind plus the protocol version it
///    is pinned to. The version is load-bearing: without it a step slug
///    cannot be resolved against the Workflow row this packet is
///    actually running, and in-flight packets stay pinned to the
///    version they were admitted under.
/// 2. WHICH STEP IS READY AND WHO OWNS IT — every step by `spec_slug`
///    (the stable handle; `title` is prose that gets reworded between
///    versions), its kind, its holder, and the `authority_role` that
///    says who may complete it. The ready/active one is marked.
/// 3. WHAT DOES IT LINK TO — one line per metadata key, value fitted.
///
/// THE METADATA IS KEYS-WITH-PREVIEWS, NOT A DUMP. A 4 KB `claim` used
/// to push the steps off the screen. Every key still appears, so an
/// absence here is a real absence, and the line naming `--json` says
/// where the untouched copy is — a reduction that hides where the full
/// record went is the defect CLAUDE.md §Diagnosis describes.
///
/// Reads every shape through [`crate::envelope`]: the id, the steps and
/// the receipt each have ONE definition there, so this renderer cannot
/// mis-key a row the way fifteen throwaway parsers did.
pub(crate) fn render_packet(job: &Value, width: usize) -> String {
    let g = |k: &str| job.get(k).and_then(Value::as_str).unwrap_or("-");
    let version = job
        .get("workflow_version")
        .and_then(Value::as_i64)
        .map(|v| format!(" v{v}"))
        .unwrap_or_default();
    let mut out = format!(
        "{}\n{}{}   status {}   priority {}   opened {}\n{}\nowner {}   subject {}/{}\n",
        crate::envelope::job_id(job).unwrap_or("-"),
        g("kind"),
        version,
        g("status"),
        g("priority"),
        g("opened_on"),
        crate::envelope::job_title(job).unwrap_or("-"),
        g("owner_id"),
        job.pointer("/subject/subject_kind")
            .and_then(Value::as_str)
            .unwrap_or("-"),
        job.pointer("/subject/id")
            .and_then(Value::as_str)
            .unwrap_or("-"),
    );

    let steps = crate::envelope::steps(job);
    if !steps.is_empty() {
        let lines: Vec<crate::envelope::StepLine> = steps
            .iter()
            .map(|s| crate::envelope::step_line(s))
            .collect();
        let sw = lines
            .iter()
            .map(|l| l.slug.chars().count())
            .max()
            .unwrap_or(0);
        let kw = lines
            .iter()
            .map(|l| l.kind.chars().count())
            .max()
            .unwrap_or(0);
        let stw = lines
            .iter()
            .map(|l| l.status.chars().count())
            .max()
            .unwrap_or(0);
        // The holder column is padded too, so the authority that follows
        // it lands in one column instead of sliding per row.
        let aw = lines
            .iter()
            .map(|l| l.assignee.as_deref().unwrap_or("—").chars().count())
            .max()
            .unwrap_or(0);
        out.push_str(&format!(
            "\nsteps ({})   * = where the packet is now\n",
            lines.len()
        ));
        for l in &lines {
            let authority = l
                .authority_role
                .as_deref()
                .map(|r| format!("  authority {r}"))
                .unwrap_or_default();
            let row = format!(
                "  {} {:stw$}  {:sw$}  {:kw$}  {:aw$}{}",
                if l.now { '*' } else { ' ' },
                l.status,
                l.slug,
                l.kind,
                l.assignee.as_deref().unwrap_or("—"),
                authority,
            );
            // `trim_end` before fitting: a step with no authority would
            // otherwise carry the holder column's padding as trailing
            // blanks, which diff tools and copy-paste both pick up.
            out.push_str(&fit(row.trim_end(), width));
            out.push('\n');
        }
    }

    // A gate receipt, when the packet carries one. A verdict must name
    // what failed (CLAUDE.md §Diagnosis), so a red lists its fails here
    // rather than sending the reader to re-derive them.
    if let Some((step, r)) = crate::envelope::receipt(job) {
        // The head gets its own line and is never shortened: a sha is
        // for copying, and a truncated one invites the hand-authored
        // half-sha that memory `never-write-a-sha-you-did-not-read` is
        // about. Everything else fits on one line.
        out.push_str(&format!(
            "\nreceipt (on \"{step}\")\n    {}   mode {}   {} check(s), {} fail(s)\n    head {}\n",
            r.verdict,
            r.mode,
            r.checks,
            r.fails.len(),
            r.head,
        ));
        for f in &r.fails {
            out.push_str(&fit(&format!("    {f}"), width));
            out.push('\n');
        }
    }

    let md = job.get("metadata").cloned().unwrap_or_else(|| json!({}));
    let keys = crate::envelope::metadata_lines(&md, width.saturating_sub(4));
    out.push_str(&format!(
        "\nmetadata ({} key(s))   values fitted — --json prints the full copy\n",
        keys.len()
    ));
    for k in keys {
        out.push_str(&format!("    {k}\n"));
    }
    out
}

/// A station's queue: the station's own facts, then one line per packet.
///
/// WHY THE STATION'S FACTS COME FIRST. A queue depth alone does not say
/// whether the queue is healthy — the discipline that orders it and the
/// WIP limit that holds it are what make a number mean something, and
/// they are on the envelope already. And the page size is stated next to
/// the real `total`, because a truncated page answers a smaller question
/// without saying so (memory: `a-limit-is-not-a-filter`).
///
/// An EMPTY queue says so in words. A header with no rows under it reads
/// identically to a failed read, which is precisely the
/// indistinguishable-from-a-true-negative class this verb exists to end.
///
/// A body with no rows array REFUSES (backlog 7b7e0529): read as zero
/// rows, a station answering an error envelope printed "empty", which is
/// that same class one layer down.
pub(crate) fn station_table(body: &Value, width: usize) -> Result<String> {
    let g = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or("-");
    let total = body.get("total").and_then(Value::as_u64).unwrap_or(0);
    let rows = crate::train::rows(Some(body.clone()))?;
    let discipline = body
        .get("discipline")
        .and_then(Value::as_array)
        .map(|d| {
            d.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_else(|| "-".to_string());
    let wip = match body.get("wip_limit").and_then(Value::as_u64) {
        Some(n) => format!("{n}"),
        None => "none".to_string(),
    };
    let mut out = format!(
        "{}   {}   discipline {discipline}   wip {wip}{}\n{} packet(s) at the station, {} on this page\n",
        g("station"),
        g("kind"),
        if body.get("over_limit").and_then(Value::as_bool) == Some(true) {
            "   OVER LIMIT"
        } else {
            ""
        },
        total,
        rows.len(),
    );
    if rows.is_empty() {
        out.push_str("  (empty — the station holds nothing)\n");
        return Ok(out);
    }
    let kw = rows
        .iter()
        .map(|r| r.get("kind").and_then(Value::as_str).unwrap_or("?").len())
        .max()
        .unwrap_or(0);
    for r in &rows {
        // `id`, via `envelope` — a station row spells it `id` and an
        // assignments row `job_id`, and asking the wrong one reported a
        // filed packet as NOT VISIBLE on this very surface.
        let id = crate::envelope::job_id(r).unwrap_or("????????");
        let line = format!(
            "  {}  {:kw$}  {:8}  {}  {}",
            &id[..8.min(id.len())],
            r.get("kind").and_then(Value::as_str).unwrap_or("?"),
            r.get("priority").and_then(Value::as_str).unwrap_or("?"),
            r.get("opened_on").and_then(Value::as_str).unwrap_or("-"),
            crate::envelope::job_title(r).unwrap_or("?"),
        );
        out.push_str(&fit(&line, width));
        out.push('\n');
    }
    Ok(out)
}

/// What the packet holds NOW for each key the patch sent — the whole
/// point of the verb. Returns the report and whether every key took;
/// a 204 that changed nothing must fail loudly, not print "patched".
pub(crate) fn confirm_patch(now: &Value, sent: &Value) -> (String, bool) {
    let mut out = String::new();
    let mut all_took = true;
    let empty = serde_json::Map::new();
    let sent_obj = sent.as_object().unwrap_or(&empty);
    for (k, v) in sent_obj {
        let current = now.get(k);
        if v.is_null() {
            match current {
                None => out.push_str(&format!("  {k}: removed\n")),
                Some(c) => {
                    all_took = false;
                    out.push_str(&format!(
                        "! {k}: sent null but the packet still holds {c}\n"
                    ));
                }
            }
        } else {
            match current {
                Some(c) if c == v => out.push_str(&format!("  {k}: {}\n", fit(&c.to_string(), 80))),
                Some(c) => {
                    all_took = false;
                    out.push_str(&format!(
                        "! {k}: wrote {} but the packet holds {}\n",
                        fit(&v.to_string(), 60),
                        fit(&c.to_string(), 60)
                    ));
                }
                None => {
                    all_took = false;
                    out.push_str(&format!(
                        "! {k}: wrote a value but the packet has no such key\n"
                    ));
                }
            }
        }
    }
    (out, all_took)
}

fn width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(100)
}

/// One page of the job list.
pub(crate) const RESOLVE_PAGE: usize = 500;

/// How many closed rows a prefix lookup will read before giving up.
///
/// The list is `ORDER BY opened_on DESC, created_at DESC`, so "the
/// newest 500 closed" is the newest 500 BY OPENING DATE — and a busy
/// day closes a thousand rows (gate-runs, chores, ops-requests), so a
/// packet opened this morning and closed tonight sat past page one and
/// `boss job get <prefix>` said "no job matches … give the full uuid if
/// it is older than that" for a packet closed seconds earlier, three
/// times in one evening (71d2334c). Ten pages covers about a week of
/// this deployment's closings; the refusal names the depth it read.
const RESOLVE_CLOSED_MAX: usize = 5_000;

/// How many closed rows to read for a prefix lookup: the whole set when
/// it fits, else the bound. Pure, so the arithmetic is pinned.
pub(crate) fn closed_rows_to_read(total: usize) -> usize {
    total.min(RESOLVE_CLOSED_MAX)
}

/// Fetch rows to resolve a reference against: open first (most lookups
/// are live work), then closed — page by page, newest opening date
/// first, up to [`RESOLVE_CLOSED_MAX`] rows — stopping at the first
/// page that matches.
pub(crate) async fn fetch_and_resolve(
    http: &boss_core::machine_token::Client,
    job_ref: &str,
) -> Result<String> {
    if looks_like_uuid(job_ref) {
        return Ok(job_ref.to_string());
    }
    let page = |status: &'static str, offset: usize| async move {
        let path = format!("/api/jobs?status={status}&limit={RESOLVE_PAGE}&offset={offset}");
        crate::gate::api(http, reqwest::Method::GET, &path, None).await
    };
    resolve_through(page, job_ref).await
}

/// [`fetch_and_resolve`] over any page reader — the seam its tests go
/// through, so the paging rules are pinned without a socket.
pub(crate) async fn resolve_through<F, Fut>(page: F, job_ref: &str) -> Result<String>
where
    F: Fn(&'static str, usize) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    let id_of = |row: &Value| {
        row.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("matched a job with no id")
    };
    let open = crate::train::rows(page("open", 0).await?)?;
    match resolve(&open, job_ref) {
        Ok(row) => return id_of(row),
        Err(e) if e.to_string().starts_with("no job matches") => {}
        Err(e) => return Err(e),
    }
    let mut read = 0usize;
    let mut to_read = RESOLVE_CLOSED_MAX;
    while read < to_read {
        // A page without its `total` is refused, never read as zero
        // closed jobs (backlog 10776b6c): zero set the depth to nothing
        // and the verb answered "no job matches" for a packet it had
        // not looked for.
        let body = page("closed", read)
            .await?
            .context("a closed-jobs page answered no JSON body")?;
        to_read = closed_rows_to_read(crate::train::list_total(&body)?);
        let rows = crate::train::rows(Some(body))?;
        if rows.is_empty() {
            break;
        }
        read += rows.len();
        match resolve(&rows, job_ref) {
            Ok(row) => return id_of(row),
            Err(e) if e.to_string().starts_with("no job matches") => continue,
            Err(e) => return Err(e),
        }
    }
    bail!(
        "no job matches {job_ref:?} in the newest {} open or the newest {read} closed (by \
         opening date) — give the full uuid if it is older than that",
        open.len()
    )
}

pub async fn get(job_ref: &str, raw: bool) -> Result<()> {
    let http = crate::gate::machine_client()?;
    let id = fetch_and_resolve(&http, job_ref).await?;
    let job = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .context("the job read returned no body")?;
    if raw {
        println!("{}", serde_json::to_string_pretty(&job)?);
    } else {
        print!("{}", render_packet(&job, width()));
    }
    Ok(())
}

/// `boss job station <name>` — what is queued at a station.
///
/// `--json` prints the NORMALIZED rows rather than the raw body, which
/// is the opposite choice from `get --json` and deliberate: a machine
/// reading a station wants one spelling for the packet's id, and the
/// raw body is one `boss-api` call away if it wants the envelope.
pub async fn station(name: &str, raw: bool) -> Result<()> {
    let http = crate::gate::machine_client()?;
    let body = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/stations/{name}/queue"),
        None,
    )
    .await?
    .with_context(|| format!("the station read for {name:?} returned no body"))?;
    if raw {
        let packets: Vec<Value> = crate::train::rows(Some(body.clone()))?
            .iter()
            .map(|r| {
                json!({
                    "id": crate::envelope::job_id(r),
                    "kind": r.get("kind"),
                    "status": r.get("status"),
                    "priority": r.get("priority"),
                    "opened_on": r.get("opened_on"),
                    "owner_id": r.get("owner_id"),
                    "title": crate::envelope::job_title(r),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "station": body.get("station"),
                "kind": body.get("kind"),
                "discipline": body.get("discipline"),
                "wip_limit": body.get("wip_limit"),
                "over_limit": body.get("over_limit"),
                "total": body.get("total"),
                "on_this_page": packets.len(),
                "packets": packets,
            }))?
        );
    } else {
        print!("{}", station_table(&body, width())?);
    }
    Ok(())
}

/// RFC 3986 unreserved set — everything but `A-Z a-z 0-9 - . _ ~` is
/// percent-encoded. A `--where` document goes into ONE query value, so
/// the `{ " : , /` of its JSON, and any `&` or `+` inside a value, must
/// not be read as query syntax on the far side.
pub(crate) const QUERY_VALUE: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// The flat containment document `--where key=value` pairs compose:
/// one object, string values, everything after the FIRST `=` is the
/// value (a branch or a probe text carries `=` of its own). What is
/// composed then passes the server's own rule for `metadata=`
/// (`where_containment`), so nothing this door sends is a 400.
///
/// Composed as WIRE TEXT, not as a `Value`, because of the one thing
/// only the text can show: a repeated key. A JSON object holds a key
/// once, so `--where k=1 --where k=2` read into a map is `{"k":"2"}`
/// with the first value gone without a word. Writing the pairs out as
/// the JSON this door would send — repeats and all — and handing that
/// to the server's own parser is what lets the server refuse it here,
/// in its own sentence. Until 2026-09-14 the composer caught the repeat
/// itself and said "names k twice" while the server said "key k
/// repeated" for the same document: one rule, two wordings, no pin
/// (backlog a452b11a).
pub(crate) fn where_object(wheres: &[String]) -> Result<Value> {
    let pairs = wheres
        .iter()
        .map(|w| match w.split_once('=') {
            Some((k, v)) if !k.is_empty() => Ok(format!("{}:{}", json!(k), json!(v))),
            _ => bail!("--where takes key=value, e.g. --where branch=feat/x — got {w:?}"),
        })
        .collect::<Result<Vec<_>>>()?;
    where_containment(&format!("{{{}}}", pairs.join(",")))
}

/// Judge the `--where` wire text by the server's own rule for
/// `metadata=` — `boss_jobs::metadata_containment::parse`, the ONE
/// definition and the very function the 400 comes out of, not a
/// restatement of it (until 2026-09-14 this side decided the shape
/// alone, related to the server's by a comment; backlog 88a3b072).
/// Refused HERE, before the round trip, with the sentence the 400 would
/// carry; the only word this door adds is the name of its own flag.
fn where_containment(text: &str) -> Result<Value> {
    boss_jobs::metadata_containment::parse(text)
        .map(Value::Object)
        .map_err(|why| anyhow!("--where {why}"))
}

/// Validate a `--has` key by the server's own rule for `metadata_has`
/// — `boss_jobs::metadata_key`, the ONE definition, not a copy of it
/// (a copy is what lived here until 2026-09-14; backlog b46e9d8e).
/// Refused HERE, before the round trip, with the sentence the 400 would
/// carry; the only word this door adds is the name of its own flag.
fn has_key(key: &str) -> Result<&str> {
    boss_jobs::metadata_key::check(key).map_err(|why| anyhow!("--has {why}"))
}

/// The query `boss job list` sends. Pure, so what reaches the wire is
/// pinned: `--where` pairs become one url-encoded `metadata=` document,
/// `--has` becomes `metadata_has=`, and asking for neither sends the
/// query this verb has always sent.
///
/// WHY. `GET /api/jobs` learned `metadata=` (containment) and
/// `metadata_has=` on #366, and one day later every operator and every
/// builder brief was still `boss-api GET ... | jq` over a PAGE — the
/// shape `a-limit-is-not-a-filter` names — because the terminal door
/// could only say kind/status/limit (backlog 58eef0f5).
pub(crate) fn list_query(
    kind: Option<&str>,
    status: &str,
    limit: u32,
    wheres: &[String],
    has: &[String],
) -> Result<String> {
    let mut path = format!("/api/jobs?status={status}&limit={limit}");
    if let Some(k) = kind {
        path.push_str(&format!("&kind={k}"));
    }
    if !wheres.is_empty() {
        let doc = where_object(wheres)?.to_string();
        path.push_str(&format!(
            "&metadata={}",
            percent_encoding::utf8_percent_encode(&doc, QUERY_VALUE)
        ));
    }
    match has {
        [] => {}
        [one] => path.push_str(&format!("&metadata_has={}", has_key(one)?)),
        // The API takes ONE key. Sending two would keep whichever the
        // query parser saw last and drop the other without a word.
        many => bail!(
            "--has takes one key per run (the API's metadata_has filters on a \
             single top-level key) — got {}: {}",
            many.len(),
            many.iter()
                .map(|k| format!("{k:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
    Ok(path)
}

/// The list's closing line. When the page is smaller than the answer
/// it says so — `showing N of TOTAL` — because a truncated page that
/// looks complete answers a smaller question without telling the
/// reader (memory: `a-limit-is-not-a-filter`).
pub(crate) fn list_footer(shown: usize, total: Option<u64>) -> String {
    match total {
        Some(t) if t > shown as u64 => format!("boss job: showing {shown} of {t}"),
        _ => format!("boss job: {shown} row(s)"),
    }
}

pub async fn list(
    kind: Option<String>,
    status: String,
    limit: u32,
    wheres: Vec<String>,
    has: Vec<String>,
) -> Result<()> {
    let path = list_query(kind.as_deref(), &status, limit, &wheres, &has)?;
    let http = crate::gate::machine_client()?;
    let body = crate::gate::api(&http, reqwest::Method::GET, &path, None).await?;
    let total = body
        .as_ref()
        .and_then(|b| b.get("total"))
        .and_then(Value::as_u64);
    let rows = crate::train::rows(body)?;
    let w = width();
    for r in &rows {
        println!("{}", list_line(r, w));
    }
    println!("{}", list_footer(rows.len(), total));
    Ok(())
}

/// Kinds whose INPUT LANE is a question — work-originating packets,
/// the ones `boss channels` reads. Filing one without saying where it
/// came from is refused (c5dc81a1): the filer knows the origin, and the
/// keyword classifier that used to guess it missed 252 of 310 items
/// measured on 2026-09-20, climbing to 91% missed on 09-19. A kind that
/// is not work-originating has no lane to state.
const LANE_IS_REQUIRED_OF: [&str; 1] = ["backlog-item"];

/// The lane this filing records, validated against the one vocabulary
/// (`channels::FILEABLE_LANES`). `Ok(None)` means nothing to merge —
/// either the kind has no lane, or the metadata file already carries
/// one. Refuses BEFORE the POST, the way an unnamed actor is refused,
/// so the fix arrives instead of a packet nobody can classify.
fn resolve_channel(kind: &str, flag: Option<&str>, md: &Option<Value>) -> Result<Option<String>> {
    let in_md = md
        .as_ref()
        .and_then(|m| m.get(crate::channels::RECORDED_KEY))
        .and_then(Value::as_str)
        .map(str::to_string);
    let vocabulary = || {
        crate::channels::FILEABLE_LANES
            .iter()
            .map(|c| c.label())
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let check = |lane: &str| -> Result<()> {
        if crate::channels::InputChannel::parse(lane).is_none() {
            bail!("{lane} is not an input lane — one of: {}", vocabulary());
        }
        Ok(())
    };
    if let Some(lane) = flag {
        check(lane)?;
        if let Some(said) = &in_md
            && said != lane
        {
            bail!(
                "--channel {lane} disagrees with the metadata's {} {said} — say it once",
                crate::channels::RECORDED_KEY
            );
        }
        return Ok(Some(lane.to_string()));
    }
    if let Some(lane) = &in_md {
        check(lane)?;
        // Already in the body the caller hands us; nothing to merge.
        return Ok(None);
    }
    if LANE_IS_REQUIRED_OF.contains(&kind) {
        bail!(
            "a {kind} must say which lane it came in through: --channel <{}>",
            vocabulary()
        );
    }
    Ok(None)
}

/// What a filing records about its own origin, beside the lane:
/// `--source` and `--area` (design 3036296f mechanism D; the rules are
/// in `item_source`).
pub struct Origin {
    pub source: Option<String>,
    pub area: Option<String>,
}

pub async fn file(
    kind: &str,
    title: &str,
    priority: Option<String>,
    metadata: Option<std::path::PathBuf>,
    subject_id: Option<String>,
    channel: Option<String>,
    origin: Origin,
) -> Result<()> {
    let md = match &metadata {
        Some(p) => Some(
            serde_json::from_str(
                &std::fs::read_to_string(p)
                    .with_context(|| format!("reading metadata {}", p.display()))?,
            )
            .with_context(|| format!("{} is not JSON", p.display()))?,
        ),
        None => None,
    };
    let filing = Filing {
        kind,
        title,
        priority: priority.as_deref(),
        subject_id: subject_id.as_deref(),
        channel: channel.as_deref(),
        metadata: md,
    };
    let report = file_on(&crate::steps::Wire::live()?, filing, origin).await?;
    println!("{report}");
    Ok(())
}

/// One filing, as `boss job file` was handed it — the metadata file
/// already read.
pub(crate) struct Filing<'a> {
    pub kind: &'a str,
    pub title: &'a str,
    pub priority: Option<&'a str>,
    pub subject_id: Option<&'a str>,
    pub channel: Option<&'a str>,
    pub metadata: Option<Value>,
}

/// [`file`] on an explicit wire — the base and the caller as values, so
/// a test drives the whole verb against the REAL jobs router and its
/// admission (backlog 443eedc9: the CLI and admission each owned
/// `opened_by`, and the two landed cars refused every filing).
/// Answers the report the verb prints.
pub(crate) async fn file_on(
    wire: &crate::steps::Wire,
    filing: Filing<'_>,
    origin: Origin,
) -> Result<String> {
    let Filing {
        kind,
        title,
        priority,
        subject_id,
        channel,
        metadata: md,
    } = filing;
    // The lane is RECORDED here, at the point it is known, rather than
    // inferred later from the title's words (c5dc81a1).
    let mut md = md;
    if let Some(lane) = resolve_channel(kind, channel, &md)? {
        match md.get_or_insert_with(|| json!({})).as_object_mut() {
            Some(o) => {
                o.insert(crate::channels::RECORDED_KEY.to_string(), json!(lane));
            }
            None => bail!("--metadata must be a JSON object to carry the input lane"),
        }
    }
    // What produced it, and where it lands — recorded as data, and
    // refused for a finding that names nothing (9b473d4a). Judged on
    // the lane as it now stands, flag or metadata alike, and every
    // check that needs no socket runs before the one that does.
    let lane = md
        .as_ref()
        .and_then(|m| m.get(crate::channels::RECORDED_KEY))
        .and_then(Value::as_str)
        .map(str::to_string);
    let source =
        crate::item_source::requested(kind, lane.as_deref(), origin.source.as_deref(), &md)?;
    let area = crate::item_source::area(origin.area.as_deref(), &md)?;
    // The packet is owned by whoever filed it. This used to stamp the
    // train conductor's id on every hand-filed packet — the same
    // mis-attribution `completed_by` exposed on steps (backlog
    // 5083d6f5). Resolved BEFORE the POST so an unnamed caller is
    // refused with the fix rather than filing under automation.
    let owner = wire.signer(&reqwest::Method::POST, "/api/jobs")?;
    let source = match &source {
        Some(s) => {
            let get =
                |path: String| async move { wire.call(reqwest::Method::GET, &path, None).await };
            Some(crate::item_source::pin(get, s).await?)
        }
        None => None,
    };
    // Who FILED it is not sent: admission stamps `opened_by` from the
    // signed caller, as the login door resolved it, and refuses a body
    // naming anyone else (958edca6). Sending it here refused every
    // filing on 2026-09-27 (backlog 443eedc9) — see item_source::merge.
    let md = crate::item_source::merge(md, source, area)?;
    let origin_keys = [crate::item_source::SOURCE_KEY, crate::item_source::AREA_KEY];
    let recorded: serde_json::Map<String, Value> = origin_keys
        .iter()
        .filter_map(|k| md.get(*k).map(|v| (k.to_string(), v.clone())))
        .collect();
    let body = envelope(kind, title, priority, subject_id, &owner, Some(md));
    let created = wire
        .call(reqwest::Method::POST, "/api/jobs", Some(body))
        .await?
        .context("the create returned no body")?;
    let id = created
        .get("id")
        .and_then(Value::as_str)
        .context("the create returned no id — refusing to call that filed")?
        .to_string();

    // A 201 is a claim; the read-back is the fact.
    let job = wire
        .call(reqwest::Method::GET, &format!("/api/jobs/{id}"), None)
        .await?
        .context("created a job the API will not read back")?;
    // The origin is the point of the filing's record, so it is read back
    // like a patch's keys: a packet that lost it is not called filed.
    let (report, all_took) = confirm_patch(
        job.get("metadata").unwrap_or(&Value::Null),
        &Value::Object(recorded),
    );
    if !all_took {
        bail!("filed {id}, but its origin did not land as sent:\n{report}");
    }
    // The filer as admission recorded it — read back, not assumed: it is
    // the resolved actor, which may not be the login this verb signed with.
    let filer = job
        .get("metadata")
        .and_then(|m| m.get(crate::item_source::OPENED_BY_KEY))
        .map_or_else(|| "(not recorded)".to_string(), Value::to_string);
    Ok(format!(
        "boss job: filed {id}  \"{}\" — confirmed by reading it back\n  {}: {filer}\n{}",
        job.get("title").and_then(Value::as_str).unwrap_or("?"),
        crate::item_source::OPENED_BY_KEY,
        report.trim_end()
    ))
}

pub async fn patch(job_ref: &str, path: &std::path::Path) -> Result<()> {
    let http = crate::gate::machine_client()?;
    let sent: Value = serde_json::from_str(
        &std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
    )
    .with_context(|| format!("{} is not JSON", path.display()))?;
    if !sent.is_object() {
        bail!("a metadata patch must be a JSON object of key -> value (null removes)");
    }
    let id = fetch_and_resolve(&http, job_ref).await?;
    crate::gate::api(
        &http,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{id}/metadata"),
        Some(sent.clone()),
    )
    .await?;

    // The status code said yes; the packet is the authority.
    let job = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .context("could not read the patched job back")?;
    let now = job.get("metadata").cloned().unwrap_or_else(|| json!({}));
    let (report, all_took) = confirm_patch(&now, &sent);
    print!("{report}");
    if !all_took {
        bail!(
            "the API answered 204 but the packet does not hold what was sent — \
             a silent no-op (write-once field, or a key the API ignores)"
        );
    }
    println!("boss job: {id} patched — confirmed by reading it back");
    Ok(())
}

/// `--to 3` or `--to v3` — the version the way `boss job get` prints it
/// (`kind v3`) or bare.
pub(crate) fn parse_version(s: &str) -> Result<i32> {
    s.trim()
        .trim_start_matches(['v', 'V'])
        .parse::<i32>()
        .with_context(|| format!("--to {s:?} is not a protocol version (e.g. 3 or v3)"))
}

/// A judged move as an operator reads it: from, to, the verdict, each
/// obstacle by step, then what the move writes — each step re-projected
/// with what changed on it and what it kept, and each step inserted.
/// Pure, over the body `GET /api/jobs/{id}/convert` answers (the POST's
/// answer carries the same two lists).
pub(crate) fn render_move(body: &Value) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let n = |k: &str| body.get(k).and_then(Value::as_i64).unwrap_or_default();
    let list = |k: &str| {
        body.get(k)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let words = |v: &Value| {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    let _ = writeln!(out, "v{} -> v{}", n("from"), n("to"));
    for o in list("obstacles") {
        let step = o
            .get("step")
            .and_then(Value::as_str)
            .unwrap_or("(protocol)");
        let why = o.get("reason").and_then(Value::as_str).unwrap_or("?");
        let _ = writeln!(out, "  refused  {step}: {why}");
    }
    for r in list("reprojected") {
        let step = r.get("step").and_then(Value::as_str).unwrap_or("?");
        let _ = write!(out, "  re-project  {step}: {}", words(&r["changed"]));
        if r.get("kept").is_some() {
            let _ = write!(out, "  (kept as written: {})", words(&r["kept"]));
        }
        out.push('\n');
    }
    for i in list("inserted") {
        let step = i.get("step").and_then(Value::as_str).unwrap_or("?");
        let _ = writeln!(out, "  insert  {step}");
    }
    out
}

/// `boss job convert <packet> [--to vN] [--dry-run]` — move a packet to
/// another version of its protocol, or preview the move (design
/// 7cf202a9 Q1; the CLI twin of `/api/jobs/{id}/convert`).
///
/// The dry run is a READ (`GET`), so it can be run across a cohort
/// before anyone moves a packet (Q5) and needs no actor. The move is a
/// write, and the API refuses it to anyone who may not publish a
/// protocol version (Q4). Either way the verb asks the preview first
/// and prints it, so a refusal names each obstacle by step instead of
/// arriving as a 409 inside an error line.
///
/// CONFIRMATION OVER STATUS CODES: after a move, the packet is read back
/// and the verb FAILS unless it is pinned to the target and its
/// `repins` record grew by one.
pub async fn convert(job_ref: &str, to: Option<&str>, dry_run: bool) -> Result<()> {
    let to = to.map(parse_version).transpose()?;
    let http = crate::gate::machine_client()?;
    let id = fetch_and_resolve(&http, job_ref).await?;
    let query = to.map(|v| format!("?to_version={v}")).unwrap_or_default();
    let preview = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}/convert{query}"),
        None,
    )
    .await?
    .context("the conversion preview returned no body")?;
    if preview.get("reason").is_some() {
        println!(
            "boss job convert: {id} — {} (v{})",
            preview["reason"].as_str().unwrap_or("nothing to do"),
            preview["workflow_version"]
        );
        return Ok(());
    }
    print!("{}", render_move(&preview));
    let convertible = preview.get("convertible").and_then(Value::as_bool) == Some(true);
    if dry_run {
        println!(
            "boss job convert: {id} — dry run, nothing written ({})",
            if convertible {
                "convertible"
            } else {
                "refused"
            }
        );
        return Ok(());
    }
    if !convertible {
        bail!(
            "{id} cannot be moved to v{} — the obstacles above name each step",
            preview["to"]
        );
    }

    let before = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .context("could not read the packet before moving it")?;
    let body = match to {
        Some(v) => json!({ "to_version": v }),
        None => json!({}),
    };
    crate::gate::api(
        &http,
        reqwest::Method::POST,
        &format!("/api/jobs/{id}/convert"),
        Some(body),
    )
    .await?;

    // The status code said yes; the packet is the authority.
    let after = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .context("could not read the moved packet back")?;
    let repins = |job: &Value| {
        job.pointer("/metadata/repins")
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
    };
    if after["workflow_version"] != preview["to"] || repins(&after) != repins(&before) + 1 {
        bail!(
            "the API answered the move but the packet does not show it: pinned v{}, {} repins \
             record(s) (was {})",
            after["workflow_version"],
            repins(&after),
            repins(&before)
        );
    }
    println!(
        "boss job convert: {id} moved to v{} — confirmed by reading it back",
        after["workflow_version"]
    );
    Ok(())
}

/// PURE: the `outcome` a CLOSED packet's metadata owes — derived from
/// its completed declared terminal under the version it is pinned to,
/// by the one rule every close uses
/// ([`boss_jobs::WorkflowSpec::completed_terminal_outcome`]) — or
/// `None` when it already records it, or the refusal. Never invented:
/// a packet that closed with no completed terminal has no outcome to
/// derive, and a recorded outcome that disagrees is not overwritten.
pub(crate) fn owed_outcome(
    job: &Value,
    spec: &boss_jobs::WorkflowSpec,
) -> std::result::Result<Option<String>, String> {
    let id = job.get("id").and_then(Value::as_str).unwrap_or("?");
    let id = &id[..8.min(id.len())];
    let status = job.get("status").and_then(Value::as_str).unwrap_or("?");
    if status != "closed" {
        return Err(format!(
            "packet {id} is {status} — an outcome is written by the close, and this repairs \
             only a close that lost it"
        ));
    }
    let steps = job
        .get("steps")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let derived = spec
        .completed_terminal_outcome(steps.iter().map(|s| {
            (
                s.get("sort_order")
                    .and_then(Value::as_i64)
                    .and_then(|n| i32::try_from(n).ok())
                    .unwrap_or(-1),
                s.get("status").and_then(Value::as_str) == Some("completed"),
            )
        }))
        .ok_or_else(|| {
            format!(
                "packet {id} closed with no completed declared terminal under {} v{} — there \
                 is no outcome to derive, and none is invented",
                spec.kind, spec.version
            )
        })?;
    match job.pointer("/metadata/outcome").and_then(Value::as_str) {
        Some(recorded) if recorded == derived => Ok(None),
        Some(recorded) => Err(format!(
            "packet {id} records outcome {recorded:?} but its completed terminal declares \
             {derived:?} — a recorded outcome is not overwritten; read the packet"
        )),
        None => Ok(Some(derived.to_string())),
    }
}

/// `boss job outcome <packet> [--dry-run]` — write the `outcome` a
/// closed packet lost, re-derived from its completed terminal step
/// (228c9a7d). Car 6b23d135 closed through `disproved` and a racing
/// catch-all close erased the outcome its terminal close had stamped;
/// the server no longer writes that close without it, and this is the
/// door for a packet closed before it did — not a hand PATCH of a value
/// someone read off the steps. Confirmed by reading the packet back.
pub async fn outcome(job_ref: &str, dry_run: bool) -> Result<()> {
    let http = crate::gate::machine_client()?;
    let id = fetch_and_resolve(&http, job_ref).await?;
    let job = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .context("could not read the packet")?;
    let kind = job
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("packet {id} has no kind"))?;
    let version = job
        .get("workflow_version")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("packet {id} is pinned to no version"))?;
    let spec: boss_jobs::WorkflowSpec = serde_json::from_value(
        crate::gate::api(
            &http,
            reqwest::Method::GET,
            &format!("/api/workflows/{kind}/versions/{version}"),
            None,
        )
        .await?
        .with_context(|| format!("{kind} v{version} is not in the registry"))?,
    )
    .with_context(|| format!("{kind} v{version} did not read as a protocol version"))?;
    let owed = owed_outcome(&job, &spec).map_err(|e| anyhow!("boss job outcome: REFUSED — {e}"))?;
    let Some(owed) = owed else {
        println!("boss job outcome: {id} already records its terminal's outcome — nothing to do");
        return Ok(());
    };
    if dry_run {
        println!(
            "boss job outcome: {id} — dry run, would write outcome {owed:?} (its completed \
             terminal under {kind} v{version})"
        );
        return Ok(());
    }
    let sent = json!({ "outcome": owed });
    crate::gate::api(
        &http,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{id}/metadata"),
        Some(sent.clone()),
    )
    .await?;
    let after = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .context("could not read the packet back")?;
    let (report, took) = confirm_patch(
        &after.get("metadata").cloned().unwrap_or_else(|| json!({})),
        &sent,
    );
    print!("{report}");
    if !took {
        bail!("the API answered but packet {id} does not hold the outcome that was sent");
    }
    println!(
        "boss job outcome: {id} records outcome {owed:?}, derived from its completed terminal \
         — confirmed by reading it back"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    /// 228c9a7d: the outcome a closed packet owes is its completed
    /// terminal's — 6b23d135's shape — and nothing else is written: an
    /// open packet, a close with no completed terminal, and a recorded
    /// outcome that disagrees are each refused by name; one that agrees
    /// is nothing to do.
    #[test]
    fn a_lost_outcome_is_re_derived_from_the_completed_terminal() {
        use serde_json::json;
        let spec: boss_jobs::WorkflowSpec = serde_json::from_value(json!({
            "kind": "ship-a-change", "version": 3, "status": "active",
            "label": "Ship a change", "category": "engineering",
            "subject_kinds": ["custom"], "owning_team": "platform",
            "created_at": "2026-09-24T00:00:00Z",
            "steps": [
                {"title": "review", "kind": "task", "ready_when": "true"},
                {"title": "merged", "kind": "outcome", "ready_when": "true",
                 "terminal": {"outcome": "merged"}},
                {"title": "disproved", "kind": "outcome", "ready_when": "true",
                 "terminal": {"outcome": "disproved"}},
            ],
        }))
        .unwrap();
        let car = json!({
            "id": "6b23d135-1bde-47cc-9bd5-5612c741b9f2", "status": "closed",
            "metadata": {"merged": "true", "disproved": "true"},
            "steps": [
                {"sort_order": 0, "status": "completed"},
                {"sort_order": 1, "status": "skipped"},
                {"sort_order": 2, "status": "completed"},
            ],
        });
        assert_eq!(
            super::owed_outcome(&car, &spec),
            Ok(Some("disproved".to_string()))
        );

        let mut repaired = car.clone();
        repaired["metadata"]["outcome"] = json!("disproved");
        assert_eq!(super::owed_outcome(&repaired, &spec), Ok(None));

        let mut other = car.clone();
        other["metadata"]["outcome"] = json!("merged");
        let e = super::owed_outcome(&other, &spec).unwrap_err();
        assert!(e.contains("not overwritten"), "{e}");

        let mut open = car.clone();
        open["status"] = json!("open");
        let e = super::owed_outcome(&open, &spec).unwrap_err();
        assert!(e.contains("6b23d135 is open"), "{e}");

        let mut catch_all = car.clone();
        catch_all["steps"][2]["status"] = json!("skipped");
        let e = super::owed_outcome(&catch_all, &spec).unwrap_err();
        assert!(e.contains("none is invented"), "{e}");
    }

    #[test]
    fn a_version_reads_bare_or_as_printed() {
        assert_eq!(super::parse_version("3").unwrap(), 3);
        assert_eq!(super::parse_version("v8").unwrap(), 8);
        assert!(super::parse_version("latest").is_err());
    }

    /// The preview names every step the move touches, and what it kept.
    #[test]
    fn a_rendered_move_names_each_step_and_what_it_kept() {
        let out = super::render_move(&serde_json::json!({
            "from": 1, "to": 3, "convertible": true, "obstacles": [],
            "reprojected": [
                {"step": "measure", "changed": ["`procedure`"]},
                {"step": "file", "changed": [], "kept": ["`procedure`"]},
            ],
            "inserted": [{"step": "draft-design"}],
        }));
        assert!(out.starts_with("v1 -> v3\n"), "{out}");
        assert!(out.contains("re-project  measure: `procedure`"), "{out}");
        assert!(out.contains("(kept as written: `procedure`)"), "{out}");
        assert!(out.contains("insert  draft-design"), "{out}");
    }

    /// A refusal names each obstacle by step.
    #[test]
    fn a_rendered_refusal_names_the_step() {
        let out = super::render_move(&serde_json::json!({
            "from": 2, "to": 7, "convertible": false,
            "obstacles": [{"step": "measure", "reason": "required field `x` added"}],
        }));
        assert!(
            out.contains("refused  measure: required field `x` added"),
            "{out}"
        );
    }

    /// A closed page with no `total` is refused, never read as zero
    /// closed jobs (backlog 10776b6c). Counted as 0, it set the read
    /// depth to 0, the loop never ran, and the verb said "no job
    /// matches … give the full uuid" for a packet it had not looked for.
    #[tokio::test]
    async fn a_closed_page_without_a_total_refuses_rather_than_matching_nothing() {
        let why = super::resolve_through(
            |status, _offset| async move {
                anyhow::Ok(Some(match status {
                    "open" => serde_json::json!({"data": [], "total": 0}),
                    _ => serde_json::json!({"data": [{"id": "abcdef12-0000-4000-8000-000000000000"}]}),
                }))
            },
            "abcdef12",
        )
        .await
        .expect_err("a page that cannot say how many closed jobs exist decides nothing")
        .to_string();
        assert!(why.contains("total"), "{why}");
        assert!(!why.contains("no job matches"), "{why}");
    }

    /// And with its total, the same page resolves the prefix.
    #[tokio::test]
    async fn a_closed_page_with_its_total_resolves_the_prefix() {
        let id = super::resolve_through(
            |status, _offset| async move {
                anyhow::Ok(Some(match status {
                    "open" => serde_json::json!({"data": [], "total": 0}),
                    _ => serde_json::json!({
                        "data": [{"id": "abcdef12-0000-4000-8000-000000000000"}],
                        "total": 1
                    }),
                }))
            },
            "abcdef12",
        )
        .await
        .expect("resolved");
        assert_eq!(id, "abcdef12-0000-4000-8000-000000000000");
    }

    /// A STATION THAT DID NOT ANSWER IS NOT AN EMPTY ONE (backlog
    /// 7b7e0529): a body with no rows array refuses rather than printing
    /// "(empty — the station holds nothing)".
    #[test]
    fn a_station_body_with_no_rows_refuses_rather_than_reading_empty() {
        let body = serde_json::json!({"error": "no such station", "total": 0});
        let why = super::station_table(&body, 120)
            .expect_err("an error envelope is not an empty queue")
            .to_string();
        assert!(why.contains("cannot be read as zero"), "{why}");
    }

    #[test]
    fn a_prefix_lookup_reads_the_whole_closed_set_when_it_fits_and_the_bound_when_not() {
        assert_eq!(super::closed_rows_to_read(0), 0);
        assert_eq!(super::closed_rows_to_read(1_200), 1_200);
        assert_eq!(super::closed_rows_to_read(25_008), 5_000);
    }

    use super::*;

    /// c5dc81a1: the input channel is RECORDED at filing, not guessed
    /// from the title's words later. A backlog item is refused without
    /// one, and the refusal names the vocabulary rather than leaving the
    /// filer to find it - the filer knows the origin and a heuristic
    /// never will.
    #[test]
    fn a_backlog_item_must_name_the_lane_it_came_in_through() {
        let said = resolve_channel("backlog-item", None, &None)
            .unwrap_err()
            .to_string();
        assert!(said.contains("--channel"), "{said}");
        // Every fileable lane is offered, from the one list.
        for lane in crate::channels::FILEABLE_LANES {
            assert!(
                said.contains(lane.label()),
                "{} missing from: {said}",
                lane.label()
            );
        }
        // `unclassified` is never an answer a filer may give.
        assert!(!said.contains("unclassified"), "{said}");

        // The flag records the lane.
        assert_eq!(
            resolve_channel("backlog-item", Some("discovery-while-working"), &None).unwrap(),
            Some("discovery-while-working".to_string())
        );
        // A metadata file that already carries it satisfies the
        // requirement - the same field, written through the other door.
        let md = Some(json!({"input_channel": "roadmap"}));
        assert_eq!(resolve_channel("backlog-item", None, &md).unwrap(), None);
        // A misspelled lane is refused, not filed as a lane nothing reads.
        let said = resolve_channel("backlog-item", Some("vibes"), &None)
            .unwrap_err()
            .to_string();
        assert!(said.contains("vibes"), "{said}");
        // Other kinds are unaffected: only work-originating packets have
        // an input lane to state.
        assert_eq!(resolve_channel("ops-request", None, &None).unwrap(), None);
    }

    #[test]
    fn the_envelope_defaults_land_and_explicit_values_win() {
        let e = envelope("backlog-item", "t", None, None, "actor:x", None);
        assert_eq!(e["tags"], json!([]));
        assert_eq!(e["subject"]["id"], "bosspipeline");
        assert_eq!(e["subject"]["subject_kind"], "custom");
        assert_eq!(e["owner_id"], "actor:x");
        assert_eq!(e["status"], "open");
        assert_eq!(e["priority"], "standard");
        assert_eq!(e["metadata"], json!({}));

        let e = envelope(
            "k",
            "t",
            Some("urgent"),
            Some("boss-dev-0"),
            "a",
            Some(json!({"x": 1})),
        );
        assert_eq!(e["priority"], "urgent");
        assert_eq!(e["subject"]["id"], "boss-dev-0");
        assert_eq!(e["metadata"]["x"], 1);
    }

    /// The create handler stamps `metadata.opened_at` — the precise
    /// filing instant behind the one-day `opened_on` — ONLY when its
    /// clock owns the date, i.e. when the body carries no `opened_on`
    /// (boss-jobs http/jobs.rs). This envelope sent `today`, so every
    /// packet filed by `boss job file` / `boss ops` defeated the stamp
    /// (backlog a7a07ffb: 9f9fa486, a7a07ffb and ops-request 25cb2f71
    /// all lacked it, and timing one meant reading its event stream).
    /// `boss gate` already leaves the date to the clock (gate.rs). A
    /// caller that MEANS a backdated packet passes `opened_on` itself.
    #[test]
    fn the_envelope_leaves_the_open_date_to_the_api_clock() {
        let e = envelope("backlog-item", "t", None, None, "actor:x", None);
        assert!(
            e.get("opened_on").is_none(),
            "`opened_on` must be left to the create handler's clock, \
             or the packet gets no `opened_at`: {e}"
        );
    }

    #[test]
    fn a_short_prefix_is_refused_an_eight_char_one_resolves() {
        let rows = vec![json!({"id": "abcdef12-3456-7890-aaaa-bbbbccccdddd",
                               "title": "x", "metadata": {}})];
        assert!(resolve(&rows, "abcdef1").is_err());
        assert!(resolve(&rows, "abcdef12").is_ok());
        // A full uuid never reaches resolve — it goes straight to the API.
        assert!(looks_like_uuid("abcdef12-3456-7890-aaaa-bbbbccccdddd"));
        assert!(!looks_like_uuid("abcdef12"));
        assert!(!looks_like_uuid("abcdef12-3456-7890-aaaa-bbbbccccddd?"));
    }

    #[test]
    fn ambiguity_is_refused_not_chosen() {
        let rows = vec![
            json!({"id": "abcdef12-aaaa-1111-2222-333344445555", "title": "one", "metadata": {}}),
            json!({"id": "abcdef12-bbbb-1111-2222-333344445555", "title": "two", "metadata": {}}),
        ];
        let e = resolve(&rows, "abcdef12").unwrap_err().to_string();
        assert!(e.contains("2 jobs match"), "{e}");
        // A refusal that does not LIST the matches sends the reader back
        // to the API to find out what it was ambiguous between — and
        // picking one silently is worse still. Both, by id and title.
        assert!(e.contains("abcdef12-aaaa-1111-2222-333344445555"), "{e}");
        assert!(e.contains("abcdef12-bbbb-1111-2222-333344445555"), "{e}");
        assert!(e.contains("one") && e.contains("two"), "{e}");

        // A branch is the other way a packet is referred to in practice,
        // and two cars can share one id prefix OR one branch name. The
        // listing must read from whichever key the row carries its id
        // under, so an assignment row is listed rather than shown as
        // `?` (envelope::job_id is the one definition).
        let by_branch = vec![
            json!({"job_id": "11111111-aaaa-1111-2222-333344445555",
                   "job_title": "car one", "metadata": {"branch": "feat/x"}}),
            json!({"job_id": "22222222-bbbb-1111-2222-333344445555",
                   "job_title": "car two", "metadata": {"branch": "feat/x"}}),
        ];
        let e = resolve(&by_branch, "feat/x").unwrap_err().to_string();
        assert!(e.contains("2 jobs match"), "{e}");
        assert!(e.contains("11111111-aaaa-1111-2222-333344445555"), "{e}");
        assert!(e.contains("22222222-bbbb-1111-2222-333344445555"), "{e}");
        assert!(e.contains("car one") && e.contains("car two"), "{e}");
    }

    #[test]
    fn patch_confirmation_reports_what_the_packet_now_holds() {
        // Every key took: the happy path reads as a receipt.
        let (out, ok) = confirm_patch(
            &json!({"a": "x", "b": 2}),
            &json!({"a": "x", "b": 2, "gone": null}),
        );
        assert!(ok, "{out}");
        assert!(out.contains("gone: removed"));

        // The silent-204 case this verb exists for: a key the API
        // ignored is a FAILURE, named per key.
        let (out, ok) = confirm_patch(&json!({"a": "x"}), &json!({"a": "y", "new": 1}));
        assert!(!ok);
        assert!(out.contains("! a: wrote"), "{out}");
        assert!(out.contains("! new:"), "{out}");

        // Sent null but the key survived: also a failure.
        let (out, ok) = confirm_patch(&json!({"stuck": 1}), &json!({"stuck": null}));
        assert!(!ok);
        assert!(out.contains("still holds"), "{out}");
    }

    /// `GET /api/jobs/d44f6152-…` trimmed to three steps. Captured with
    /// `boss-api` on 2026-09-11.
    fn packet() -> Value {
        json!({
          "id": "d44f6152-47e8-467b-b2ee-f5c99b741b98",
          "kind": "backlog-item",
          "workflow_version": 5,
          "subject": { "subject_kind": "custom", "id": "bosspipeline" },
          "title": "There is no read verb for a packet or a queue",
          "owner_id": "emp-david",
          "status": "open",
          "priority": "standard",
          "opened_on": "2026-09-11",
          "metadata": { "channel": "monitoring", "claim": "a claim\nwith a second line" },
          "steps": [
            { "kind": "trigger", "title": "Filed to the backlog", "spec_slug": "filed",
              "assignee_id": null, "status": "completed", "metadata": {} },
            { "kind": "task", "title": "Measure the claim, choose a route",
              "spec_slug": "triage", "assignee_id": "claude@algedonic.dev",
              "status": "ready", "metadata": { "authority_role": "platform-admin" } },
            { "kind": "task", "title": "Re-measure the claim", "spec_slug": "measure",
              "assignee_id": null, "status": "pending",
              "metadata": { "authority_role": "platform-admin" } }
          ]
        })
    }

    #[test]
    fn the_render_answers_what_kind_which_step_is_ready_and_who_owns_it() {
        let out = render_packet(&packet(), 120);

        // WHAT KIND OF PACKET: the kind AND the protocol version it is
        // pinned to. Without the version a reader cannot resolve a step
        // slug against the Workflow row the packet is actually running.
        assert!(out.contains("backlog-item v5"), "{out}");
        assert!(out.contains("status open"), "{out}");
        assert!(out.contains("owner emp-david"), "{out}");
        assert!(out.contains("custom/bosspipeline"), "{out}");

        // WHICH STEP IS READY: marked, and the slug is the handle —
        // `title` is prose that gets reworded between versions.
        let now: Vec<&str> = out
            .lines()
            .filter(|l| l.trim_start().starts_with('*'))
            .collect();
        assert_eq!(now.len(), 1, "exactly one step is where it is: {out}");
        assert!(now[0].contains("triage"), "{out}");

        // WHO OWNS IT: the holder, and who is ALLOWED to complete it.
        assert!(now[0].contains("claude@algedonic.dev"), "{out}");
        assert!(now[0].contains("platform-admin"), "{out}");

        // Every step appears with its slug and kind, not just the ready
        // one — a reader asks "what comes next" as often as "what now".
        for slug in ["filed", "triage", "measure"] {
            assert!(out.contains(slug), "missing step {slug}: {out}");
        }
        assert!(out.contains("trigger"), "a step's kind rides too: {out}");
        assert!(out.contains("steps (3)"), "{out}");

        // Narrow terminals: every table row fits, so the step list stays
        // one packet-step per line rather than wrapping into a mess. The
        // TITLE line is content and is left whole — truncating what the
        // packet is about to make a column line up is the wrong trade.
        let narrow = render_packet(&packet(), 64);
        for line in narrow.lines().filter(|l| l.starts_with("  ")) {
            assert!(line.chars().count() <= 64, "{line:?} in:\n{narrow}");
        }
    }

    #[test]
    fn the_render_lists_metadata_keys_and_says_where_the_full_copy_is() {
        let out = render_packet(&packet(), 120);
        assert!(out.contains("metadata (2 key(s))"), "{out}");
        assert!(out.contains("channel"), "{out}");
        assert!(
            out.contains("a claim with a second line"),
            "a folded value, not a dropped second line: {out}"
        );
        // A reduction that does not say where the full copy is throws
        // away the only copy (CLAUDE.md §Diagnosis).
        assert!(out.contains("--json"), "{out}");
    }

    #[test]
    fn a_gate_run_renders_its_verdict_and_names_what_failed() {
        let red = json!({
          "id": "326f1532-d981-41dc-a080-0fcf6033e0b0",
          "kind": "gate-run", "workflow_version": 3, "status": "closed",
          "title": "Gate fix/x", "metadata": { "branch": "fix/x" },
          "steps": [{
            "spec_slug": "record-verdict", "kind": "gate-verdict",
            "title": "Record the receipt", "status": "completed",
            "metadata": { "receipt": "{\"verdict\":\"red\",\"mode\":\"full\",\"head\":\"abc1234\",\"checks\":[{\"name\":\"test\",\"result\":\"fail\",\"seconds\":90}],\"fails\":[\"test: one case panicked\"]}" }
          }]
        });
        let out = render_packet(&red, 120);
        assert!(out.contains("receipt"), "{out}");
        assert!(out.contains("red"), "{out}");
        assert!(out.contains("head abc1234"), "{out}");
        // A verdict someone must go re-derive is not a verdict.
        assert!(out.contains("test: one case panicked"), "{out}");
        // A packet with no receipt renders no receipt line at all.
        assert!(!render_packet(&packet(), 120).contains("receipt"));
    }

    #[test]
    fn the_station_table_names_the_stations_own_facts_and_one_row_per_packet() {
        // `GET /api/stations/q.platform-admin.task/queue`, trimmed to two
        // rows. Captured with `boss-api` on 2026-09-11: the rows are
        // packet envelopes keyed `id`, and they carry NO steps.
        let body = json!({
          "station": "q.platform-admin.task", "kind": "constraint",
          "discipline": ["priority", "age"], "wip_limit": null,
          "over_limit": false, "lens": null, "upstream": null, "total": 61,
          "data": [
            { "id": "ac356440-2aab-4d0d-af09-93a6f6488ba6",
              "kind": "rotate-a-credential", "status": "open", "priority": "urgent",
              "opened_on": "2026-09-03", "owner_id": "emp-david",
              "title": "Maiden rotation: the broker's first self-managed token (v2)",
              "metadata": {} },
            { "id": "290a5269-c026-4ea7-bd11-1410521aa391",
              "kind": "backlog-item", "status": "open", "priority": "urgent",
              "opened_on": "2026-09-10", "owner_id": "emp-david",
              "title": "ESTATE ALARM: disk_tight:w-1 persisted", "metadata": {} }
          ]
        });
        let out = station_table(&body, 120).expect("a station body with rows");

        // The station's own facts, which are the reason to ask a station
        // rather than list jobs: depth, discipline, and the WIP limit.
        assert!(out.contains("q.platform-admin.task"), "{out}");
        assert!(out.contains("constraint"), "{out}");
        assert!(out.contains("priority, age"), "{out}");

        // A LIMIT IS NOT A FILTER: the page shows 2 of 61 and must say
        // so, or the reader answers a smaller question without knowing.
        assert!(out.contains("61"), "the station's real depth: {out}");
        assert!(out.contains('2'), "and how many this page holds: {out}");

        // One line per packet, keyed `id` — the spelling that made a
        // filed design-doc read as NOT VISIBLE on its own station.
        assert!(out.contains("ac356440"), "{out}");
        assert!(out.contains("290a5269"), "{out}");
        assert!(out.contains("rotate-a-credential"), "{out}");
        assert!(out.lines().all(|l| l.chars().count() <= 120), "{out}");

        // An EMPTY queue says it is empty. A table with no rows under a
        // header reads identically to a failed read, which is the whole
        // defect class this verb exists for.
        let empty = json!({"station": "loading-dock", "kind": "batch",
                           "discipline": ["priority"], "total": 0, "data": []});
        let empty = station_table(&empty, 120).expect("an empty queue is an answer");
        assert!(empty.contains("empty"), "{empty}");
    }

    #[test]
    fn list_lines_cut_to_width_and_carry_the_short_id() {
        let row = json!({"id": "abcdef12-3456-7890-aaaa-bbbbccccdddd",
                         "kind": "backlog-item", "status": "open",
                         "title": "a very long title that will not fit in a narrow terminal at all"});
        let line = list_line(&row, 40);
        assert!(line.starts_with("abcdef12  backlog-item  open"), "{line}");
        assert_eq!(line.chars().count(), 40, "{line}");
        assert!(line.ends_with("..."));
        // Wide enough: untouched.
        assert!(!list_line(&row, 200).ends_with("..."));
    }

    #[test]
    fn two_where_values_compose_one_containment_object() {
        // `--where branch=feat/x --where merged=true` is ONE `metadata=`
        // document, url-encoded, so the server's `@>` reads both at once.
        let path = list_query(
            Some("ship-a-change"),
            "open",
            50,
            &["branch=feat/x".to_string(), "merged=true".to_string()],
            &[],
        )
        .unwrap();
        assert!(path.starts_with("/api/jobs?status=open&limit=50"), "{path}");
        assert!(path.contains("&kind=ship-a-change"), "{path}");
        // RFC 3986: `{` `"` `:` `/` `,` `}` are all percent-encoded, so a
        // value carrying `&` or `+` can never split the query.
        assert!(
            path.contains(
                "&metadata=%7B%22branch%22%3A%22feat%2Fx%22%2C%22merged%22%3A%22true%22%7D"
            ),
            "{path}"
        );
        assert!(!path.contains("metadata_has"), "{path}");
        // Nothing asked: nothing sent. The old query, byte for byte.
        assert_eq!(
            list_query(None, "open", 50, &[], &[]).unwrap(),
            "/api/jobs?status=open&limit=50"
        );
    }

    #[test]
    fn a_where_value_keeps_everything_after_the_first_equals() {
        let doc = where_object(&["note=a=b".to_string()]).unwrap();
        assert_eq!(doc, json!({"note": "a=b"}));
        // A key or value the JSON must escape composes valid wire text,
        // so the server's parser reads back exactly what was typed.
        let doc = where_object(&[r#"say "hi"=back\slash"#.to_string()]).unwrap();
        assert_eq!(doc, json!({"say \"hi\"": "back\\slash"}));
        // The same key twice cannot both hold in one flat object; one
        // would win silently. Refused instead — in the SERVER's words:
        // the wire text this door would send, repeats and all, is judged
        // by `boss_jobs::metadata_containment::parse`, so the sentence is
        // the 400's byte for byte behind the flag's name. Until
        // 2026-09-14 this side had its own "names k twice" sentence for
        // the same fact (backlog a452b11a).
        let e = where_object(&["k=1".to_string(), "k=2".to_string()]).unwrap_err();
        let server = boss_jobs::metadata_containment::parse(r#"{"k":"1","k":"2"}"#).unwrap_err();
        assert_eq!(e.to_string(), format!("--where {server}"));
        assert!(e.to_string().ends_with("key \"k\" repeated"), "{e}");
        // No `=` at all is not a filter; it is refused with the shape.
        let e = where_object(&["branch".to_string()]).unwrap_err();
        assert!(e.to_string().contains("key=value"), "{e}");
        let e = where_object(&["=v".to_string()]).unwrap_err();
        assert!(e.to_string().contains("key=value"), "{e}");
    }

    #[test]
    fn a_where_document_is_judged_by_the_servers_containment_rule() {
        // What `--where` composes passes the server's rule for
        // `metadata=` — the ONE definition both doors call
        // (`boss_jobs::metadata_containment`), so a document the terminal
        // sends is one the server takes. Until 2026-09-14 the two sides
        // each decided the shape alone, related by a comment (backlog
        // 88a3b072).
        let doc = where_object(&["branch=feat/x".to_string(), "merged=true".to_string()]).unwrap();
        assert_eq!(
            boss_jobs::metadata_containment::check(&doc).map(Value::Object),
            Ok(doc)
        );
        // Should this door ever build something wider than flat strings,
        // it is refused HERE, before the round trip, with the terminal's
        // flag in front of byte for byte what the 400 would carry.
        for bad in [json!({"a": {"b": "c"}}), json!({"n": 1}), json!(["a"])] {
            let said = where_containment(&bad.to_string()).unwrap_err().to_string();
            assert!(
                said.starts_with("--where must be"),
                "the terminal names ITS parameter in front of the rule: {bad}: {said}"
            );
            assert!(
                said.contains(boss_jobs::metadata_containment::RULE),
                "{bad}: {said}"
            );
            let server = boss_jobs::metadata_containment::check(&bad).unwrap_err();
            assert_eq!(said, format!("--where {server}"), "{bad}");
        }
    }

    #[test]
    fn has_is_one_identifier_validated_like_the_server() {
        let path = list_query(None, "open", 50, &[], &["proof_probe".to_string()]).unwrap();
        assert!(path.ends_with("&metadata_has=proof_probe"), "{path}");
        // The server's own rule sentence — read from the ONE definition
        // both doors call (`boss_jobs::metadata_key`), not retyped here,
        // so the terminal refuses BEFORE the round trip and says the
        // same thing the 400 would. Until 2026-09-14 this string and the
        // check behind it were a second copy (backlog b46e9d8e).
        for bad in ["steps.0", "9lives", "a-b", ""] {
            let e = list_query(None, "open", 50, &[], &[bad.to_string()]).unwrap_err();
            let said = e.to_string();
            assert!(
                said.starts_with("--has must be"),
                "the terminal names ITS parameter in front of the rule: {bad:?}: {said}"
            );
            assert!(
                said.contains(boss_jobs::metadata_key::RULE),
                "{bad:?}: {said}"
            );
            assert!(
                said.contains("dotted paths are not walked"),
                "{bad:?}: {said}"
            );
            // The whole tail after the parameter's name is the shared
            // check's own word — byte for byte what the 400 carries.
            let server = boss_jobs::metadata_key::check(bad).unwrap_err();
            assert_eq!(said, format!("--has {server}"), "{bad:?}");
        }
        // The API takes ONE `metadata_has`; two is refused with a sentence
        // rather than one of them silently dropped.
        let e = list_query(None, "open", 50, &[], &["a".to_string(), "b".to_string()]).unwrap_err();
        assert!(e.to_string().contains("one"), "{e}");
        assert!(e.to_string().contains("\"a\""), "{e}");
        assert!(e.to_string().contains("\"b\""), "{e}");
    }

    #[test]
    fn the_list_footer_says_when_the_page_is_smaller_than_the_answer() {
        // A LIMIT IS NOT A FILTER: 50 rows of 184 must say so.
        assert_eq!(list_footer(50, Some(184)), "boss job: showing 50 of 184");
        // The whole answer fits: the old line, unchanged.
        assert_eq!(list_footer(3, Some(3)), "boss job: 3 row(s)");
        // A body with no `total` cannot claim more than it shows.
        assert_eq!(list_footer(3, None), "boss job: 3 row(s)");
    }

    // ------------------------------------------------------------------
    // `boss job file` against the REAL admission (backlog 443eedc9).
    //
    // Measured 2026-09-27: two cars landed within hours, each right on
    // its own gate — the CLI stamped `opened_by` from BOSS_ACTOR, and
    // admission (958edca6) began stamping it from the signed caller and
    // refusing a create whose value names anyone else. On the live
    // stack the login door rewrites `claude@algedonic.dev` to
    // `agent-claude` before admission reads it, so the CLI's value
    // named "someone else" and EVERY backlog-item filing answered 422.
    // Neither car's tests could see the other half. This drives the
    // verb's own create path through the jobs router, its admission
    // and the login door as the binary mounts it, in memory — so the
    // two can never contradict again without a red here (CLAUDE.md §9a).
    // ------------------------------------------------------------------
    mod filing_meets_admission {
        use std::sync::Arc;

        use boss_jobs::registry::seedable_platform_workflows;
        use boss_jobs::{
            InMemoryJobs, InMemoryWorkflows, JobFilter, JobsRepository, WorkflowRegistry,
        };
        use boss_policy_client::{FakePolicyClient, PolicyClient};
        use serde_json::json;

        use super::super::{Filing, Origin, file_on};

        /// The login the CLI signs with (BOSS_ACTOR on the pod) and the
        /// actor the agents registry maps it to — the two spellings the
        /// live refusal named.
        const LOGIN: &str = "claude@algedonic.dev";
        const AGENT: &str = "agent-claude";

        async fn serve() -> (String, Arc<InMemoryJobs>) {
            let jobs = Arc::new(InMemoryJobs::new());
            let policy: Arc<dyn PolicyClient> = Arc::new(
                // The SHIPPED platform grants (`default_rules`), not a
                // hand-picked few: the human-road test below must fail if
                // the platform-admin a person signs in as cannot take a
                // step of it (backlog b7b02024, re-review D1).
                FakePolicyClient::builder().with_default_rules().build(),
            );
            let bus = boss_testing::RecordingEventBus::new();
            let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
            let publisher = boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs");
            let kinds = Arc::new(InMemoryWorkflows::new());
            for spec in seedable_platform_workflows() {
                kinds.seed(spec).expect("seed platform kind");
            }
            let state = boss_jobs::http::JobsApiState {
                kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
                ..boss_jobs::http::JobsApiState::minimal(
                    jobs.clone(),
                    bus,
                    publisher.clone(),
                    policy,
                    Arc::new(boss_clock_client::WallClockClient),
                )
            };
            // The login door, layered the way boss_jobs_api.rs mounts it.
            let agents =
                Arc::new(boss_jobs::agents::InMemoryAgents::new().with_agent(AGENT, [LOGIN]));
            let door = Arc::new(boss_jobs::agents::LoginDoor::new(agents, publisher));
            let app = boss_jobs::http::router(state).layer(axum::middleware::from_fn_with_state(
                door,
                boss_jobs::agents::resolve_login,
            ));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            (format!("http://{addr}"), jobs)
        }

        fn wire(base: String) -> crate::steps::Wire {
            crate::steps::Wire::at(
                base,
                Some(crate::identity::Caller {
                    id: LOGIN.into(),
                    source: crate::identity::Source::Env,
                }),
            )
            .unwrap()
        }

        fn filing(metadata: Option<serde_json::Value>) -> Filing<'static> {
            Filing {
                kind: "backlog-item",
                title: "A backlog item filed through the verb",
                priority: None,
                subject_id: None,
                channel: Some("roadmap"),
                metadata,
            }
        }

        fn no_origin() -> Origin {
            Origin {
                source: None,
                area: Some("cli".into()),
            }
        }

        async fn stored(jobs: &InMemoryJobs) -> Vec<boss_core::job::Job> {
            jobs.list_jobs(&JobFilter::default(), 10, 0)
                .await
                .unwrap()
                .0
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn the_verb_files_and_admission_records_the_signer_as_filer() {
            let (base, jobs) = serve().await;
            let report = file_on(&wire(base), filing(None), no_origin())
                .await
                .expect("admitted");
            assert!(report.contains("boss job: filed"), "{report}");
            let rows = stored(&jobs).await;
            assert_eq!(rows.len(), 1, "one packet landed");
            let md = &rows[0].metadata;
            // Admission's spelling — the actor the login resolved to.
            assert_eq!(md["opened_by"], AGENT, "{md}");
            assert_eq!(md["input_channel"], "roadmap", "{md}");
            assert_eq!(md["area"], "cli", "{md}");
            assert!(
                report.contains(AGENT),
                "the report names the filer: {report}"
            );
        }

        /// THE OPERATOR'S ROAD, WITH NO AGENT RUNNING (backlog b7b02024,
        /// DR rule 62dac114, re-review D1). A car held for its review
        /// carries `hold_sha`, so only a release naming an agent-run
        /// whose verdict is RELEASE at its head lets it board. Every
        /// agent-run on the live stack was opened by an agent; this walks
        /// the three commands `boss release --help` names, as the HUMAN
        /// `emp-david`, through the real jobs router, its admission and
        /// the login door: `boss job file --kind agent-run`, then `boss
        /// review`, then `boss release --review`. It fails if any link
        /// refuses a human — admission of an agent-run filed by one, a
        /// verdict recorded on a run no agent opened, a release by a
        /// person — and it ends at the dock: the conductor's own `dock()`
        /// and `vouches` board the car.
        #[tokio::test(flavor = "multi_thread")]
        async fn a_human_alone_releases_a_review_held_car_through_the_three_verbs() {
            const HUMAN: &str = "emp-david";
            const CAR: &str = "c0ffee00-0000-4000-8000-00000000ca25";
            const BUILDER: &str = "b0b0b0b0-0000-4000-8000-000000000000";
            const HEAD: &str = "1111111111111111111111111111111111111111";
            const MAIN: &str = "2222222222222222222222222222222222222222";
            let (base, jobs) = serve().await;
            // The car at the dock: gated green, its review held for a
            // finding at HEAD, built by an agent run.
            let mut car = boss_core::job::Job::new(
                "ship-a-change",
                boss_core::job::Subject::new("custom", "bosspipeline"),
                "a held car",
                HUMAN,
                boss_core::job::Priority::Standard,
                chrono::NaiveDate::from_ymd_opt(2026, 9, 29).expect("a date"),
            );
            car.id = serde_json::from_value(json!(CAR)).expect("a car id");
            car.status = boss_core::job::JobStatus::Open;
            car.metadata = json!({"branch": "feat/held", "agent_run": BUILDER});
            let step = |slug: &str, status: &str, md: serde_json::Value| {
                serde_json::from_value::<boss_core::job::Step>(json!({
                    "id": uuid::Uuid::new_v4().to_string(), "job_id": CAR,
                    "title": if slug == "review" { boss_jobs::car::REVIEW } else { slug },
                    "spec_slug": slug, "status": status, "metadata": md,
                }))
                .expect("a step")
            };
            let steps = [
                step("gate", "completed", json!({})),
                step(
                    "review",
                    "ready",
                    json!({"hold": "touches an ops verb", boss_jobs::car::HOLD_SHA: HEAD}),
                ),
            ];
            let stamp = boss_core::publisher::EventStamp::new(
                "jobs",
                boss_core::actor::ActorId::Automation("test".into()),
            );
            let step_events: Vec<_> = steps
                .iter()
                .map(|s| {
                    stamp.event(
                        boss_jobs::events::STEP_CREATED,
                        boss_jobs::events::step_state_payload(s),
                    )
                })
                .collect();
            let _admitted = jobs
                .create_job_with_steps_at(&car, &steps, stamp.timestamp, &[], &step_events)
                .await
                .expect("the car stands at the dock");
            let wire = crate::steps::Wire::at(
                base,
                Some(crate::identity::Caller {
                    id: HUMAN.into(),
                    source: crate::identity::Source::Env,
                }),
            )
            .unwrap();
            let forge = |r: &str| -> anyhow::Result<String> {
                Ok(if r == "refs/heads/main" { MAIN } else { HEAD }.to_string())
            };

            // 1. `boss job file --kind agent-run --title '…'`
            let report = file_on(
                &wire,
                Filing {
                    kind: "agent-run",
                    title: "Review of feat/held by emp-david",
                    priority: None,
                    subject_id: None,
                    channel: None,
                    metadata: None,
                },
                Origin {
                    source: None,
                    area: None,
                },
            )
            .await
            .expect("admission takes an agent-run a human files");
            let run = report
                .split_whitespace()
                .skip_while(|w| *w != "filed")
                .nth(1)
                .expect("the report names the run")
                .to_string();
            // 2. `BOSS_AGENT_RUN=<run> boss review feat/held --verdict release`
            crate::review_verdict::record(&wire, "feat/held", "release", "", Some(&run), forge)
                .await
                .expect("a human records a verdict on the run they opened");
            // 3. `boss release feat/held --review <run>`
            crate::review_verdict::release(&wire, "feat/held", &run, forge)
                .await
                .expect("a human releases on that verdict");

            // The dock: the conductor's own two readers board the car.
            let after = wire.packet(CAR).await.expect("the car");
            assert!(crate::train::parked_ready(&after), "unheld: {after}");
            let never = || -> crate::mutating_verb::Judgement {
                panic!("a car with a release is not judged")
            };
            let unasked = |_: &str, _: &str| -> Result<String, String> {
                panic!("a release at the head it reviewed proves nothing further")
            };
            assert_eq!(
                crate::mutating_verb::dock(&after, HEAD, never, unasked),
                crate::mutating_verb::Dock::Check {
                    review: run.clone(),
                    reviewed: HEAD.into(),
                    carry: None,
                }
            );
            let run_packet = wire.packet(&run).await.expect("the run");
            assert_eq!(
                crate::review_verdict::vouches(&run_packet, &after, HEAD),
                Ok(()),
                "the dock's check passes a run a human opened: {run_packet}"
            );
        }

        /// The road the test above walks is the road `boss release --help`
        /// tells the operator to walk — the three verbs, in order, and the
        /// test that pins them. A help line that drifts from the chain is
        /// a dead end found at exactly the wrong moment.
        #[test]
        fn release_help_names_the_three_verbs_the_human_road_walks() {
            let help = include_str!("steps.rs");
            let at = |needle: &str| {
                help.find(needle)
                    .unwrap_or_else(|| panic!("boss release --help names {needle:?}"))
            };
            let file = at("1. boss job file --kind agent-run --title 'Review of <car> by <you>'");
            let review = at("2. BOSS_AGENT_RUN=<that id> boss review <car> --verdict release");
            let release = at("3. boss release <car> --review <that id>");
            assert!(file < review && review < release, "in order");
            assert!(
                help[release..].contains("`died`"),
                "the side effect is named"
            );
            at("a_human_alone_releases_a_review_held_car_through_the_three_verbs");
        }

        /// The CLI no longer judges the filer; admission does, once. A
        /// metadata file naming someone else still never lands.
        #[tokio::test(flavor = "multi_thread")]
        async fn a_metadata_file_naming_another_filer_is_refused_by_admission() {
            let (base, jobs) = serve().await;
            let err = file_on(
                &wire(base),
                filing(Some(json!({"opened_by": "emp-david"}))),
                no_origin(),
            )
            .await
            .expect_err("another filer");
            let said = format!("{err:#}");
            assert!(
                said.contains("422") && said.contains("emp-david") && said.contains(AGENT),
                "{said}"
            );
            assert!(stored(&jobs).await.is_empty(), "nothing landed");
        }
    }
}
