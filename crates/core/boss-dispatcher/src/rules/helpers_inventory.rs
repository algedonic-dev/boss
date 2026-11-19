//! Reorder-rule helpers — `open_po_exists`, `vendor_for`, `open_restock_exists`.
//!
//! These are the helper functions the canonical reorder-threshold rule
//! calls. The first two query the inventory-api; `open_restock_exists`
//! queries the jobs-api to dedup per-SKU on an in-flight restock Job (which
//! exists the instant it's spawned) rather than on the PO it places later.
//!
//! Implementation note: HelperResolver::call is sync because the
//! expression evaluator is sync. The matcher runs inside tokio, so
//! we use `block_in_place` + `Handle::current().block_on` to bridge
//! to async reqwest. Requires a multi-threaded tokio runtime (which
//! the dispatcher binary uses by default via #[tokio::main]).

use super::expr::{EvalError, HelperResolver, Value};

const SYSTEM_USER_HEADER: &str = r#"{"id":"automation:dispatcher","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

pub struct InventoryHelpers {
    /// Stamped with the machine token, redirects off: these reads asked
    /// the inventory and jobs ports with no token until review S2 of
    /// design 6805c764 car 2, and a port in `enforce` would have made
    /// every reorder rule's helper fail.
    client: boss_core::machine_token::Client,
    inventory_base: String,
    jobs_base: String,
}

impl InventoryHelpers {
    pub fn new(inventory_base: impl Into<String>, jobs_base: impl Into<String>) -> Self {
        Self {
            client: boss_core::machine_token::Client::build(
                reqwest::Client::builder().timeout(std::time::Duration::from_secs(3)),
            )
            .expect("async client builds"),
            inventory_base: inventory_base.into(),
            jobs_base: jobs_base.into(),
        }
    }

    fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        helper: &str,
    ) -> Result<T, EvalError> {
        // A read emits nothing, so this header changes no recorded
        // fact today. It is stamped anyway to keep the rule without
        // exceptions: "every dispatcher call declares its origin" is
        // a rule anyone can apply, whereas "…except reads, unless the
        // service logs them" is one people get wrong.
        let req = self
            .client
            .get(url)
            .header("x-boss-user", SYSTEM_USER_HEADER)
            .header("x-sim-origin", crate::next_hop::sim_origin_value())
            .send();
        let resp = tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(req))
            .map_err(|e| EvalError::HelperFailed {
                name: helper.to_string(),
                msg: format!("GET {url}: {e}"),
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(EvalError::HelperFailed {
                name: helper.to_string(),
                msg: format!("GET {url} returned {status}"),
            });
        }
        let body = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(resp.json::<T>())
        })
        .map_err(|e| EvalError::HelperFailed {
            name: helper.to_string(),
            msg: format!("decode body: {e}"),
        })?;
        Ok(body)
    }
}

impl HelperResolver for InventoryHelpers {
    fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
        match name {
            "open_po_exists" => open_po_exists(self, args),
            "vendor_for" => vendor_for(self, args),
            "open_restock_exists" => open_restock_exists(self, args),
            // Not inventory-flavored, but this is the ONE resolver the
            // runner binds and it already holds jobs_base; the module
            // outgrew its name the day a second domain needed a dedup
            // helper (design-review-spawn, dogfooding arc e556c000).
            "open_publish_exists" => open_publish_exists(self, args),
            // The generalization the one-kind guards were converging
            // on: any (kind, subject) pair, so the NEXT daily spawner
            // gets its dedup as rule data instead of another one-off
            // helper (0517387b — the sweep spawners had none at all).
            // `open_car_exists`, the sweep-car dedup, retired here on
            // 2026-09-15 when `spawn-car-on-sweep-remediated` v4 began
            // filing backlog items and asking this instead (655c5917).
            "open_job_exists" => open_job_exists(self, args),
            // A pure string test, no request: the DSL compares whole
            // values only, and the conflict trigger (design b35456ac)
            // must tell `conflict: <files>` — the conductor's reading of
            // a merge it could not rebase past — from every other reason
            // a car is skipped. It lives here because this is the one
            // resolver the runners bind.
            "starts_with" => starts_with(args),
            // Pure string builders, no request (backlog 77ac4f30): the
            // DSL has no string operator, so a spawned packet's title
            // could be a literal or one payload field and nothing that
            // names WHICH packet it is about in words — "Fold design
            // <id8> (<title>) into the decision record".
            "concat" => concat(args),
            "left" => left(args),
            other => Err(EvalError::UnknownHelper(other.to_string())),
        }
    }
}

fn first_string<'a>(args: &'a [Value], helper: &str) -> Result<&'a str, EvalError> {
    match args.first() {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(other) => Err(EvalError::TypeError {
            expected: "string sku",
            got: other.kind(),
        }),
        None => Err(EvalError::HelperFailed {
            name: helper.to_string(),
            msg: "missing required arg".into(),
        }),
    }
}

fn second_string<'a>(args: &'a [Value], helper: &str) -> Result<&'a str, EvalError> {
    match args.get(1) {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(other) => Err(EvalError::TypeError {
            expected: "string subject id",
            got: other.kind(),
        }),
        None => Err(EvalError::HelperFailed {
            name: helper.to_string(),
            msg: "missing required second arg".into(),
        }),
    }
}

/// Minimal percent-encoding for a query value, no new crate: the
/// values are slugs and repo paths, this guards the day one carries a
/// space or '&'.
fn percent_encode(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[derive(serde::Deserialize)]
struct OpenPoResponse {
    exists: bool,
}

fn open_po_exists(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let sku = first_string(args, "open_po_exists")?;
    let url = format!(
        "{}/api/inventory/items/{sku}/open-po-exists",
        h.inventory_base.trim_end_matches('/')
    );
    let r: OpenPoResponse = h.get_json(&url, "open_po_exists")?;
    Ok(Value::Bool(r.exists))
}

#[derive(serde::Deserialize)]
struct VendorForResponse {
    vendor_id: String,
}

fn vendor_for(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let sku = first_string(args, "vendor_for")?;
    let url = format!(
        "{}/api/inventory/items/{sku}/primary-vendor",
        h.inventory_base.trim_end_matches('/')
    );
    let r: VendorForResponse = h.get_json(&url, "vendor_for")?;
    Ok(Value::String(r.vendor_id))
}

#[derive(serde::Deserialize)]
struct JobsListResponse {
    data: Vec<serde_json::Value>,
}

/// The packet kind `open_publish_exists` asks about. The guard names
/// only a SUBJECT, so the kind lives here — and it is a `pub const`
/// rather than a literal in the URL below because a second reader now
/// needs the same fact: `cadence_roster::parse_guard` normalizes this
/// guard to the `(kind, subject)` question the cadence-silence sweep
/// asks, and one constant cannot drift from itself (CLAUDE.md §9a).
pub const PUBLISH_JOB_KIND: &str = "publish-to-github";

/// True if a `publish-to-github` packet for this mirror subject is
/// already open — the dedup for `publish-to-github-daily`.
///
/// Same defect class as the sweep-car dedup (e74b32a1), on a SCHEDULED rule:
/// the daily spawner asked no question, so while one packet sat at its
/// approval sign-off (which, unassigned, notified nobody — 13128a0c),
/// every morning minted another. Measured 2026-08-18: ab13f05f and
/// f4e9cdf6 open at once, one day apart, identical titles (9f0c566a).
///
/// Keyed on the packet's SUBJECT (`github-mirror`): there is one mirror,
/// so one open publish packet is the invariant. Matching on subject
/// rather than a metadata key differs from the car guard deliberately —
/// the mirror subject is the packet's declared identity, not a
/// breadcrumb stamped for the guard's benefit.
fn open_publish_exists(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let subject = first_string(args, "open_publish_exists")?;
    let url = format!(
        "{}/api/jobs?kind={PUBLISH_JOB_KIND}&status=open&limit=200",
        h.jobs_base.trim_end_matches('/')
    );
    let r: JobsListResponse = h.get_json(&url, "open_publish_exists")?;
    let exists = r.data.iter().any(|j| {
        j.get("subject")
            .and_then(|s| s.get("id"))
            .and_then(|s| s.as_str())
            == Some(subject)
    });
    Ok(Value::Bool(exists))
}

/// True if an open Job of `kind` with subject id `subject_id` exists —
/// the generic (kind, subject) dedup guard.
///
/// Born as the fix for 0517387b: every daily maintenance-sweep spawner
/// fired unconditionally, so an undischargeable obligation accumulated
/// one packet per day (5 open cluster-conformance sweeps when
/// measured). The three domain guards above each solved this for one
/// kind; a fourth one-off (`open_sweep_exists`) would have continued
/// the pattern this module's own comment calls outgrown. Keyed on the
/// packet's SUBJECT like `open_publish_exists` — for sweeps the
/// subject IS the target — and filtered server-side rather than
/// paged and scanned, so the answer costs one row.
fn open_job_exists(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let kind = first_string(args, "open_job_exists")?;
    let subject_id = second_string(args, "open_job_exists")?;
    let url = format!(
        "{}/api/jobs?kind={}&status=open&subject_id={}&limit=1",
        h.jobs_base.trim_end_matches('/'),
        percent_encode(kind),
        percent_encode(subject_id),
    );
    let r: JobsListResponse = h.get_json(&url, "open_job_exists")?;
    Ok(Value::Bool(!r.data.is_empty()))
}

/// `starts_with(subject, prefix)`: true when `subject` is a string that
/// begins with `prefix`.
///
/// The SUBJECT is payload data, and on `jobs.job.updated` — every packet
/// write in the system — most packets carry no such key, so an absent,
/// null or non-string subject is a clean `false`, never an error: an
/// error there NAKs the event toward a dead letter (the same reading
/// `Value::Absent` gives every other operator, design 7b756357). The
/// PREFIX is the rule author's, so a missing or non-string one is an
/// authoring defect and says so rather than answering false forever.
fn starts_with(args: &[Value]) -> Result<Value, EvalError> {
    let prefix = second_string(args, "starts_with")?;
    Ok(Value::Bool(match args.first() {
        Some(Value::String(s)) => s.starts_with(prefix),
        _ => false,
    }))
}

/// `concat(a, b, ...)`: the strings joined, in order.
///
/// An ABSENT operand answers `Absent`, never a string with a hole in it:
/// a spawn arg that resolved to nothing is refused by the absent-arg
/// guard in `match_event`, which names the rule and leaves its
/// neighbours on the topic firing (the 2026-08-24 incident). Folding the
/// hole into a string would hand the handler a title reading "Fold
/// design  () into ..." and the guard could not see it. Any other
/// non-string is the rule author's mistake and says so.
fn concat(args: &[Value]) -> Result<Value, EvalError> {
    if args.iter().any(|a| matches!(a, Value::Absent)) {
        return Ok(Value::Absent);
    }
    args.iter()
        .map(|a| match a {
            Value::String(s) => Ok(s.as_str()),
            other => Err(EvalError::TypeError {
                expected: "string",
                got: other.kind(),
            }),
        })
        .collect::<Result<String, _>>()
        .map(Value::String)
}

/// `left(subject, n)`: the first `n` characters of `subject` — the
/// eight-character id a person reads a packet by. Absent in, absent out,
/// for the reason `concat` gives; `n` is the author's and must be a
/// non-negative integer.
fn left(args: &[Value]) -> Result<Value, EvalError> {
    let n = match args.get(1) {
        Some(Value::Int(n)) if *n >= 0 => *n as usize,
        Some(other) => {
            return Err(EvalError::TypeError {
                expected: "non-negative int",
                got: other.kind(),
            });
        }
        None => {
            return Err(EvalError::HelperFailed {
                name: "left".into(),
                msg: "missing required arg".into(),
            });
        }
    };
    match args.first() {
        Some(Value::String(s)) => Ok(Value::String(s.chars().take(n).collect())),
        Some(Value::Absent) => Ok(Value::Absent),
        Some(other) => Err(EvalError::TypeError {
            expected: "string",
            got: other.kind(),
        }),
        None => Err(EvalError::HelperFailed {
            name: "left".into(),
            msg: "missing required arg".into(),
        }),
    }
}

/// True if an `ingredient-restock` Job for `part_sku` is already open.
///
/// The reorder rule dedups on this — per-SKU, on the in-flight restock Job
/// (which exists the instant it's spawned) — rather than on the open PO the
/// restock places much later (after its audit-stock step). That lag let a
/// cold-start burst of `inventory.item.consumed` events spawn dozens of
/// duplicate restocks for the same ingredient before any PO landed. Matches
/// on the Job's `metadata.part_sku` (stamped by the reorder rule via
/// `jobs.spawn`'s `metadata.<field>` args), since the restock's subject is
/// the vendor, not the part.
fn open_restock_exists(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let part_sku = first_string(args, "open_restock_exists")?;
    let url = format!(
        "{}/api/jobs?kind=ingredient-restock&status=open&limit=200",
        h.jobs_base.trim_end_matches('/')
    );
    let r: JobsListResponse = h.get_json(&url, "open_restock_exists")?;
    let exists = r.data.iter().any(|j| {
        j.get("metadata")
            .and_then(|m| m.get("part_sku"))
            .and_then(|s| s.as_str())
            == Some(part_sku)
    });
    Ok(Value::Bool(exists))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Value {
        Value::String(v.into())
    }

    /// The conflict trigger's reading (design b35456ac): the conductor's
    /// assembly refusal reads `conflict: <files>`, and every other reason a
    /// car carries — a dock hold, a struck car, a missing branch — does not.
    #[test]
    fn starts_with_reads_a_prefix_of_a_string() {
        let yes = starts_with(&[s("conflict: crates/a.rs, crates/b.rs"), s("conflict")]);
        assert_eq!(yes, Ok(Value::Bool(true)));
        let no = starts_with(&[s("its dock re-gate is red on test"), s("conflict")]);
        assert_eq!(no, Ok(Value::Bool(false)));
    }

    /// A key the car does not carry is not a conflict, and never an error:
    /// the rule rides `jobs.job.updated`, every packet write in the system,
    /// and an error there NAKs the event toward a dead letter.
    #[test]
    fn starts_with_answers_false_for_an_absent_or_non_string_subject() {
        for subject in [Value::Absent, Value::Null, Value::Int(3), Value::Bool(true)] {
            assert_eq!(
                starts_with(&[subject.clone(), s("conflict")]),
                Ok(Value::Bool(false)),
                "{subject:?}"
            );
        }
    }

    /// The PREFIX is the rule author's, so a missing or non-string one is
    /// authoring, and says so rather than answering false forever.
    #[test]
    fn starts_with_refuses_a_missing_or_non_string_prefix() {
        assert!(starts_with(&[s("conflict: a.rs")]).is_err());
        assert!(starts_with(&[s("conflict: a.rs"), Value::Int(1)]).is_err());
        assert!(starts_with(&[]).is_err());
    }

    /// A title built from the payload (backlog 77ac4f30): strings join in
    /// order, and an id is read by its first eight characters.
    #[test]
    fn concat_and_left_build_a_title_a_person_reads() {
        let h = InventoryHelpers::new("http://unused.invalid", "http://unused.invalid");
        let id8 = h
            .call(
                "left",
                &[s("02894662-99a0-4c1d-b194-933e0077e803"), Value::Int(8)],
            )
            .expect("left");
        assert_eq!(id8, s("02894662"));
        assert_eq!(
            h.call("concat", &[s("Fold design "), id8, s(" (A title)")]),
            Ok(s("Fold design 02894662 (A title)"))
        );
        assert_eq!(
            left(&[s("ab"), Value::Int(8)]),
            Ok(s("ab")),
            "short is whole"
        );
    }

    /// A hole stays a hole: an absent operand answers Absent, so the
    /// absent-arg guard refuses the spawn by name instead of a title with
    /// a gap in it reaching a person's queue.
    #[test]
    fn concat_and_left_keep_an_absent_operand_absent() {
        assert_eq!(concat(&[s("Fold "), Value::Absent]), Ok(Value::Absent));
        assert_eq!(left(&[Value::Absent, Value::Int(8)]), Ok(Value::Absent));
    }

    /// Anything else that is not a string is the author's mistake.
    #[test]
    fn concat_and_left_refuse_what_the_author_got_wrong() {
        assert!(concat(&[s("a"), Value::Int(1)]).is_err());
        assert!(concat(&[Value::Null]).is_err());
        assert!(left(&[s("abc")]).is_err());
        assert!(left(&[s("abc"), Value::Int(-1)]).is_err());
        assert!(left(&[Value::Int(3), Value::Int(1)]).is_err());
    }

    /// It dispatches by name through the one resolver the runner binds,
    /// with no request made — which is what the pin in
    /// `tests/sweep_spawn_guards.rs` asks of every helper a `when` names.
    #[test]
    fn the_resolver_knows_starts_with() {
        let h = InventoryHelpers::new("http://unused.invalid", "http://unused.invalid");
        assert_eq!(
            h.call("starts_with", &[s("conflict: a.rs"), s("conflict")]),
            Ok(Value::Bool(true))
        );
    }
}
