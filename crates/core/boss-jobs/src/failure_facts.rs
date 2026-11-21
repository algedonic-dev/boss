//! A gate-run records an equipment fault and a product defect as TWO
//! facts — and this is the one definition of which a red is.
//!
//! WHY (item c02fc3b3; docs/design/factory-taxonomy-v2.md T3, v1 M4; register entry
//! `an-equipment-fault-is-not-a-product-defect`). Equipment fails two
//! ways, and until this the record could write only one of them: a
//! gate-run's `record-verdict` step carried `verdict` alone, so whether a
//! `failed` was the branch or the gate drive was readable only from the
//! receipt's prose. On 2026-11-03 gate-bay-0 recorded 120-367 s of
//! test-phase io.full stall on the reds of three clean branches, and each
//! was struck as a product red (d7e5b016). So the recorder writes two
//! fields beside the verdict, from the receipt it already holds:
//!
//!   - `equipment_fault` = {kind, checks, io_full_s, wall_s, why}, kind
//!     `refused` | `lost` | `stall-deadline` | `unjudged`;
//!   - `product_defect` = {checks, entries, why}.
//!
//! Each is absent or an object; `verdict` is unchanged. A red that holds
//! both kinds records BOTH, and readers (car T3b) let `product_defect`
//! govern the strike. A gate-run recorded before this carries neither,
//! which a reader takes as "unrecorded", never as "neither happened".
//!
//! THE RULE, per verdict:
//!
//!   - `green` and `withdrawn`: neither fact. Nothing failed.
//!   - `refused`: `equipment_fault`, always. The gate declined before it
//!     judged the branch; an infrastructure refusal is not a consist
//!     failure (CLAUDE.md §Diagnosis). So is a `failed` whose receipt
//!     says `verdict: refused` (the vocabulary before ff5b9634, the way
//!     `flake` reads it too).
//!   - `lost`: `equipment_fault`, always. The environment ended before a
//!     verdict existed.
//!   - `failed` with NO READABLE VERDICT — a receipt that is not JSON,
//!     run.sh's `verdict: unreadable`, or an `error` with no `fails`
//!     ([`unjudged_because`], the shape the conductor's
//!     `train_gate::no_verdict` reads through the same function; gate-run
//!     df02648a): `equipment_fault` kind `unjudged`, naming what was
//!     unreadable. So is a `failed` that names no failure entry, no
//!     failing check and no error ([`FailedReceipt::names_nothing`]).
//!     Never `product_defect`: nothing about the tree was judged, and an
//!     immutable defect on no evidence would strike the branch for the
//!     gate's silence (decided by the conductor on c02fc3b3, 2026-11-04).
//!   - `failed`: read per failure ENTRY — the runner writes one `fails`
//!     entry per failed test (infra/gate-runner/run.sh `detail`), so one
//!     `test` check can hold a deadline beside an unrelated assertion
//!     (review f7f21501). An entry is the DRIVE's only when its own panic
//!     message matches an [`EQUIPMENT_SIGNATURES`] phrase AND its check
//!     ran in a measured interval of its own whose io.full stall reached
//!     [`EQUIPMENT_IO_FULL_SECONDS`]. Every other entry is the product's:
//!     an assertion, a message no signature names, an entry with no panic
//!     message (a roll-up of tests not named, a FAILED with no panic
//!     line), an entry naming no check, any entry of a check under the
//!     line or with no measured interval. A failing check with no entry,
//!     and a `fails` list cut at the receipt's global bound
//!     ([`RECEIPT_TOTAL_ENTRIES`], review 3eee83a8) are the product's too:
//!     the unread entries waive nothing. No evidence is not a pass.
//!     A Python unittest entry — run.sh's error-line rung quoting an
//!     `ERROR: <test> (<id>)` or `FAIL: <test> (<id>)` line, the lint
//!     runners' shape — has no panic message of its own, so it is read
//!     by the exception its traceback ends on, in the same receipt's
//!     `fails_excerpt` (backlog 05ddf1ca): the signatures apply to that
//!     exception the way they apply to a panic message, an
//!     `AssertionError` is the branch's the way a Rust assertion is, and
//!     a traceback the excerpt does not hold whole, or holds whole but
//!     cannot tell from a quoted one, waives nothing, each said as
//!     itself (backlog e27f7228). The
//!     rung's framing line ("N error line(s), first N:") is not an
//!     entry when it quotes every line it counts.
//!
//! ONE HOME (CLAUDE.md §9a). This rule was born in the conductor
//! (boss-cli `train/equipment_red.rs`, backlog 2e9a2449), judging a red
//! TRAIN from its gate receipt. It lives here now, in Tier 1, so the
//! recorder and the conductor read one definition; `classify_red` there
//! is this module's [`read_failed_receipt`] plus the train's own forge
//! half. Nothing here reads a cluster, a clock or the API: a pure
//! function of the verdict word and the receipt.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The `record-verdict` field that carries the equipment fault.
pub const EQUIPMENT_FAULT: &str = "equipment_fault";

/// The v2 name of [`EQUIPMENT_FAULT`] (station tree T3, backlog 490c12f0;
/// docs/design/factory-taxonomy-v2.md R12): a machine fault is never
/// written as a product defect. READ, not yet written. [`recorded`]
/// takes the fault under either name, and the recorder keeps writing
/// `equipment_fault`, because the last-converged build a watchdog
/// rollback lands on reads only that name, and a fault it cannot see
/// beside a `green` verdict would read to it as a clean green. The
/// retirement car flips the writer once a build that reads both has
/// converged.
pub const MACHINE_FAULT: &str = "machine_fault";

/// The `record-verdict` field that carries the product defect.
pub const PRODUCT_DEFECT: &str = "product_defect";

/// The io.full stall, in seconds within one failing check's own
/// interval, at which a timeout in that check is the drive's and not the
/// branch's. Backlog d7e5b016 proposed "about 100 s": its reds were at
/// 120 s and 367 s and its greens at 1 s and 2 s on the same head, and
/// train 2258c95d's red was at 651 s. A red at 59 s (a8483713) stays the
/// product's — under the line, so the branch is not given the benefit.
pub const EQUIPMENT_IO_FULL_SECONDS: u64 = 100;

/// One declared shape of a failure the gate DRIVE causes: its name, and
/// the lowercase phrases a panic message says when a wait ran out.
#[derive(Debug)]
pub struct EquipmentSignature {
    pub shape: &'static str,
    pub phrases: &'static [&'static str],
}

/// THE equipment signatures, declared once. No such list existed in the
/// tree before 2e9a2449 (`flake` decides a flake by a green after a red
/// at the same head, never by text), so this table is the one place the
/// rule reads, and it is deliberately narrow: each phrase states that a
/// wait RAN OUT, never that one was configured. A bare `timeout` matched
/// a `timeout=1s` field in any output, so it is not here; neither is a
/// fixture that "did not start" for its own reason — the 2258c95d
/// fixture counts because its message says "deadline expired". The
/// io.full stall that makes such a wait the drive's is measured, not
/// read from text ([`EQUIPMENT_IO_FULL_SECONDS`]). A message that is a
/// Rust assertion (`assertion …`) is the branch's whatever it quotes.
///
/// A VERB'S OWN WORDS COUNT (backlog 724007d3). Four gates on 2026-11-04
/// and 05 stalled 160-280 s past the line and were each recorded
/// product_defect because their entries said the deadline in words this
/// table did not hold — boss-step's "not answered within 2s (curl exit
/// 28; …)", an unwrapped tokio `Elapsed(())`, BOSS's "deadline
/// exhausted", a projection "is stale" past its window — and car
/// bbaa087b was struck for a test it does not touch. Each phrase added
/// then still states that a bound RAN OUT; `max-time`, which the item
/// also named, is curl's flag in this tree and states only that one was
/// set, so it is not here (`a_message_that_quotes_the_max_time_flag_is_
/// not_a_deadline`).
pub const EQUIPMENT_SIGNATURES: &[EquipmentSignature] = &[
    EquipmentSignature {
        // "port publication deadline expired" (gcp_root_tree_sh); tokio's
        // `Elapsed`, which displays "deadline has elapsed" and, unwrapped,
        // debug-prints `Elapsed(())` (gate b47c77d8's pg_locks wait);
        // "native membership replay deadline exhausted" and its siblings
        // across the membership, claims and history readers (gate
        // a71c29c3); a verb that names its own bound, boss-step's "not
        // answered within 2s" (infra/boss-step.sh, gate 1a55a341); and the
        // pre-commit statement budget, a monotonic deadline one operation
        // spends across all its statements, in the words its one producer
        // says when it runs out (boss-events statement_budget.rs; gate
        // d7b85890, backlog 21217d8a).
        shape: "deadline",
        phrases: &[
            "deadline expired",
            "deadline exceeded",
            "deadline has elapsed",
            "deadline exhausted",
            "elapsed(())",
            "not answered within",
            "pre-commit statement budget expired",
        ],
    },
    EquipmentSignature {
        // "timed out waiting for …", "Connection timed out", Python's
        // `TimeoutExpired`; curl's exit 28 (operation timed out) and
        // timeout(1)'s exit 124 as a verb reports them; and an observer
        // socket deadline as OpenSSL displays it — `HandshakeError::
        // WouldBlock` around a WANT_READ carrying the io error of a read
        // that hit its SO_RCVTIMEO (openssl 0.10 ssl/error.rs; the TLS
        // seam builder's run 83f19197).
        shape: "timeout",
        phrases: &[
            "timed out",
            "timeoutexpired",
            "curl exit 28",
            "exit 124",
            "the handshake was interrupted: a nonblocking read call would have blocked",
        ],
    },
    EquipmentSignature {
        // A read refused because what it reads aged past a wall-clock
        // freshness window: "membership projection is stale; last
        // generation …" was `HEALTHY_FOR` (5 s, in
        // membership_projection_service.rs, retired with the membership
        // replay by design 7b8ed079 car 2) running out between a refresh
        // and its read (gate a71c29c3, train ed012255). The signature stays
        // so receipts that carry the phrase are still read.
        shape: "freshness window",
        phrases: &["projection is stale"],
    },
];

/// infra/gate-runner/run.sh `TOTAL_ENTRIES`: the most `fails` entries one
/// receipt carries, the last of them the omission marker when it cut.
/// A list this long may have lost entries, so it is never read as whole.
/// Spelled twice because the runner is Python; held equal to run.sh by
/// `the_receipt_bound_is_the_runners` (CLAUDE.md §9a).
pub const RECEIPT_TOTAL_ENTRIES: usize = 40;

/// The most characters of a receipt's own words a `why` quotes. The
/// receipt itself rides beside the facts on the same step, whole, so a
/// `why` is the sentence a reader needs, never the only copy.
const WHY_QUOTE_CHARS: usize = 400;

/// How the equipment failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EquipmentFaultKind {
    /// The gate declined before it judged the branch.
    Refused,
    /// The environment ended before a verdict existed.
    Lost,
    /// A wait ran out in a check whose own interval stalled on io.full.
    StallDeadline,
    /// A red with no readable verdict: nothing about the tree was judged.
    Unjudged,
}

/// `equipment_fault` on a gate-run's `record-verdict` step. `io_full_s`
/// and `wall_s` are the stalled checks' own intervals, summed — null when
/// nothing measured them (a refusal, a lost run).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EquipmentFault {
    pub kind: EquipmentFaultKind,
    pub checks: Vec<String>,
    pub io_full_s: Option<u64>,
    pub wall_s: Option<u64>,
    pub why: String,
}

/// `product_defect` on a gate-run's `record-verdict` step: the checks it
/// names, how many failure entries were the product's, and why — the
/// first reason in receipt order, with a count of the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductDefect {
    pub checks: Vec<String>,
    pub entries: usize,
    pub why: String,
}

/// The two facts one verdict records: each absent or present.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FailureFacts {
    pub equipment_fault: Option<EquipmentFault>,
    pub product_defect: Option<ProductDefect>,
}

impl FailureFacts {
    /// The present facts as `record-verdict` fields, equipment first. A
    /// green records none, so the step carries no key for it at all.
    pub fn fields(&self) -> Vec<(&'static str, Value)> {
        let mut out = Vec::new();
        if let Some(f) = &self.equipment_fault {
            out.push((EQUIPMENT_FAULT, json!(f)));
        }
        if let Some(d) = &self.product_defect {
            out.push((PRODUCT_DEFECT, json!(d)));
        }
        out
    }

    /// The facts as ONE merge-door write (review 2e95708a R1): BOTH keys,
    /// every time — a present fact as its object, an absent one as null,
    /// which the step merge door deletes in the same PATCH that sets the
    /// verdict. The door keeps every key it is not sent, so a writer that
    /// named only its present fact left the other one from an interrupted
    /// earlier attempt (its PATCH landed, its completion PUT answered 409)
    /// beside the new verdict: a stale product_defect, the fact that
    /// strikes, beside a lost run's equipment_fault.
    pub fn writes(&self) -> Vec<(&'static str, Value)> {
        vec![
            (
                EQUIPMENT_FAULT,
                self.equipment_fault
                    .as_ref()
                    .map_or(Value::Null, |f| json!(f)),
            ),
            (
                PRODUCT_DEFECT,
                self.product_defect
                    .as_ref()
                    .map_or(Value::Null, |d| json!(d)),
            ),
        ]
    }

    /// Does this red strike the cars it judged? Only a product defect
    /// does — present alone or beside an equipment fault, it governs the
    /// strike (decided on c02fc3b3, Q_shape). An equipment fault with no
    /// product defect never strikes a car or owes it a re-gate.
    pub fn strikes(&self) -> bool {
        self.product_defect.is_some()
    }

    /// The equipment fault that JUDGED NOTHING (`unjudged`), when it is
    /// the whole record: no car is struck and no car is released for it
    /// (the conductor on c02fc3b3, 2026-11-04).
    pub fn unjudged(&self) -> Option<&EquipmentFault> {
        self.equipment_fault
            .as_ref()
            .filter(|f| f.kind == EquipmentFaultKind::Unjudged && self.product_defect.is_none())
    }

    /// The equipment fault that EARNS the equipment release — refused,
    /// lost or stall-deadline, with no product defect beside it.
    pub fn releasing_fault(&self) -> Option<&EquipmentFault> {
        self.equipment_fault
            .as_ref()
            .filter(|f| f.kind != EquipmentFaultKind::Unjudged && self.product_defect.is_none())
    }

    /// Which fact(s) this record carries, in words an alert title reads.
    pub fn names(&self) -> String {
        let equipment = self
            .equipment_fault
            .as_ref()
            .map(|f| format!("an equipment fault ({})", f.kind.as_str()));
        match (&self.product_defect, equipment) {
            (Some(_), Some(e)) => format!("a product defect and {e}"),
            (Some(_), None) => "a product defect".to_string(),
            (None, Some(e)) => e,
            (None, None) => "no failure fact".to_string(),
        }
    }
}

impl EquipmentFaultKind {
    /// The recorded spelling (`stall-deadline`), for a sentence.
    pub fn as_str(self) -> &'static str {
        match self {
            EquipmentFaultKind::Refused => "refused",
            EquipmentFaultKind::Lost => "lost",
            EquipmentFaultKind::StallDeadline => "stall-deadline",
            EquipmentFaultKind::Unjudged => "unjudged",
        }
    }
}

/// THE READERS' HALF (item c02fc3b3, car T3b). What a gate-run's
/// `record-verdict` step RECORDED, read back — never re-derived from the
/// receipt by each reader. The merge, the red-train alert, auto-cancel,
/// the dock's strike count and the conductor's equipment release all
/// read through this, so none can judge one gate-run differently.
///
/// `None` is UNRECORDED: the step carries neither field, which is every
/// gate-run recorded before car T3a (and every green). A reader keeps
/// its verdict-only behaviour for such a record; nothing is backfilled
/// (decided on c02fc3b3, Q_old_records). Absence is never read as
/// "neither happened".
///
/// A field that is present but will not read as its shape is still a
/// fact that was written: a `product_defect` that will not parse is a
/// product defect (it strikes — the conservative answer), and an
/// `equipment_fault` that will not parse is `unjudged` (it releases
/// nobody: no evidence is not a pass).
///
/// The fault is read under EITHER name, `equipment_fault` or its v2 name
/// [`MACHINE_FAULT`] (station tree T3). The two are one fact: both
/// present and equal read as that fact, and both present and DIFFERENT
/// read as `unjudged`, because the record then cannot say which happened.
pub fn recorded(verdict_step_metadata: &Value) -> Option<FailureFacts> {
    let field = |k: &'static str| {
        verdict_step_metadata
            .get(k)
            .filter(|v| !v.is_null())
            .map(|v| (k, v))
    };
    let (v1, v2, product) = (
        field(EQUIPMENT_FAULT),
        field(MACHINE_FAULT),
        field(PRODUCT_DEFECT).map(|(_, v)| v),
    );
    let names_disagree = matches!((v1, v2), (Some((_, a)), Some((_, b))) if a != b);
    let equipment = v1.or(v2);
    if equipment.is_none() && product.is_none() {
        return None;
    }
    let unread = |k: &str, v: &Value| {
        format!(
            "the recorded {k} could not be read as its shape: {}",
            quote(&v.to_string())
        )
    };
    let unjudged = |why: String| EquipmentFault {
        kind: EquipmentFaultKind::Unjudged,
        checks: Vec::new(),
        io_full_s: None,
        wall_s: None,
        why,
    };
    Some(FailureFacts {
        equipment_fault: equipment.map(|(k, v)| {
            if names_disagree {
                return unjudged(format!(
                    "the recorded {EQUIPMENT_FAULT} and {MACHINE_FAULT} disagree, so the record \
                     cannot say which fault happened"
                ));
            }
            serde_json::from_value(v.clone()).unwrap_or_else(|_| unjudged(unread(k, v)))
        }),
        product_defect: product.map(|v| {
            serde_json::from_value(v.clone()).unwrap_or_else(|_| ProductDefect {
                checks: Vec::new(),
                entries: 0,
                why: unread(PRODUCT_DEFECT, v),
            })
        }),
    })
}

/// One failing check judged as the drive's: its name, the io.full stall
/// and wall seconds of its own interval, and the [`EQUIPMENT_SIGNATURES`]
/// shapes its equipment entries met (`deadline, timeout` when both).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalledCheck {
    pub check: String,
    pub io_full_s: u64,
    pub wall_s: u64,
    pub marker: String,
}

/// A `failed` receipt read per failure entry: the drive's part and the
/// product's part, each in receipt order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FailedReceipt {
    /// The checks holding at least one entry that was the drive's.
    pub stalled: Vec<StalledCheck>,
    /// One sentence per [`Self::stalled`] check.
    said: Vec<String>,
    /// Why each part of the red is not the drive's, in receipt order:
    /// the cut first, then each check, then entries naming no check.
    pub defects: Vec<String>,
    /// The checks a defect names, once each.
    pub defect_checks: Vec<String>,
    /// How many failure entries were the product's.
    pub defect_entries: usize,
    /// The receipt names nothing at all: no failure entry, no failing
    /// check. Its one defect says so, which is what the conductor's
    /// `classify_red` reads (CHANGE, unchanged in car T3a); the recorder
    /// reads this flag and writes it as `unjudged` equipment instead.
    pub names_nothing: bool,
}

impl FailedReceipt {
    /// The drive's part in a sentence, or None when it holds none.
    pub fn equipment_why(&self) -> Option<String> {
        (!self.said.is_empty()).then(|| {
            format!(
                "{} (the line is {EQUIPMENT_IO_FULL_SECONDS} s)",
                self.said.join("; ")
            )
        })
    }

    /// The product's part in a sentence: its first reason, and how many
    /// more there are. None when every part was the drive's.
    pub fn defect_why(&self) -> Option<String> {
        let first = self.defects.first()?;
        Some(match self.defects.len() - 1 {
            0 => first.clone(),
            more => format!(
                "{first} (and {more} more part{} of this red not the drive's)",
                if more == 1 { "" } else { "s" }
            ),
        })
    }

    fn defect(&mut self, check: Option<&str>, entries: usize, why: String) {
        self.defects.push(why);
        self.defect_entries += entries;
        if let Some(c) = check
            && !self.defect_checks.iter().any(|d| d == c)
        {
            self.defect_checks.push(c.to_string());
        }
    }
}

/// PURE: the two facts a verdict word and its receipt record. `receipt`
/// is the receipt parsed as JSON, or None when it is not (a lost run's
/// receipt is the recorder's prose). See the module header for the rule.
pub fn failure_facts(verdict: &str, receipt: Option<&Value>) -> FailureFacts {
    let receipt_says = receipt
        .and_then(|r| r.get("verdict"))
        .and_then(Value::as_str);
    match verdict {
        "green" | "withdrawn" => FailureFacts::default(),
        "lost" => FailureFacts {
            equipment_fault: Some(EquipmentFault {
                kind: EquipmentFaultKind::Lost,
                checks: Vec::new(),
                io_full_s: None,
                wall_s: None,
                why: "the gate left no verdict about the tree: its environment ended before a \
                      receipt could be read, so it was recorded lost — an infrastructure death \
                      is not a consist failure"
                    .into(),
            }),
            product_defect: None,
        },
        "refused" => refused(receipt),
        _ if receipt_says == Some(crate::flake::REFUSED) => refused(receipt),
        // `failed`, and any word the step's enum would refuse anyway:
        // read as a red, the conservative answer.
        _ => {
            // NO READABLE VERDICT: unjudged equipment, never a product
            // defect (c02fc3b3, decided 2026-11-04).
            let Some(receipt) = receipt else {
                return unjudged(
                    "the red's receipt is not readable JSON, so nothing about the tree was \
                     judged"
                        .into(),
                );
            };
            if let Some(why) = unjudged_because(receipt) {
                return unjudged(quote(&why));
            }
            let read = read_failed_receipt(receipt);
            // A red that names no failure entry, no failing check and no
            // error evidences no defect either: unjudged, never the
            // product's (the conductor on c02fc3b3, 2026-11-04).
            if read.names_nothing {
                return unjudged(
                    "the gate's receipt names no failing check, no failure entry and no error, \
                     so nothing about the tree was judged"
                        .into(),
                );
            }
            let equipment_fault = read.equipment_why().map(|why| EquipmentFault {
                kind: EquipmentFaultKind::StallDeadline,
                checks: read.stalled.iter().map(|c| c.check.clone()).collect(),
                io_full_s: Some(read.stalled.iter().map(|c| c.io_full_s).sum()),
                wall_s: Some(read.stalled.iter().map(|c| c.wall_s).sum()),
                why,
            });
            let product_defect = read.defect_why().map(|why| ProductDefect {
                checks: read.defect_checks.clone(),
                entries: read.defect_entries,
                why,
            });
            FailureFacts {
                equipment_fault,
                product_defect,
            }
        }
    }
}

/// The equipment fault of a red that judged nothing: no check, nothing
/// measured, and why — and no product defect.
fn unjudged(why: String) -> FailureFacts {
    FailureFacts {
        equipment_fault: Some(EquipmentFault {
            kind: EquipmentFaultKind::Unjudged,
            checks: Vec::new(),
            io_full_s: None,
            wall_s: None,
            why,
        }),
        product_defect: None,
    }
}

/// PURE: why a red judged NOTHING about the tree — the receipt's own
/// sentence — or None for a receipt that judged it: a green, a refusal
/// (its own kind), or a red that names its `fails`. Gate-run df02648a
/// (2026-10-18) refused an empty diff before any check ran; run.sh wrote
/// no receipt.json, so the recorded receipt read `verdict: unreadable`
/// with the reason in `error` and no `fails` (backlog 81408a81).
///
/// ONE DEFINITION (CLAUDE.md §9a): the conductor's
/// `train_gate::no_verdict` reads a train gate's receipt through this
/// function, so the recorder's `unjudged` and the conductor's no-verdict
/// arm cannot name two different shapes or say them in two spellings.
pub fn unjudged_because(receipt: &Value) -> Option<String> {
    let fails = receipt
        .get("fails")
        .and_then(Value::as_array)
        .is_some_and(|f| !f.is_empty());
    match receipt.get("verdict").and_then(Value::as_str) {
        Some("unreadable") => unreadable_because(receipt),
        Some("green") | Some(crate::flake::REFUSED) => None,
        _ if fails => None,
        _ => receipt
            .get("error")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|e| format!("the gate's receipt names no failing check: {e}")),
    }
}

/// PURE: the sentence a receipt run.sh wrote as `verdict: unreadable`
/// carries — its `error`, or that it names none — and None for any other
/// receipt. Read by [`unjudged_because`] and by the conductor's
/// `train_gate::standing`, so the two cannot describe one run differently.
pub fn unreadable_because(receipt: &Value) -> Option<String> {
    if receipt.get("verdict").and_then(Value::as_str) != Some("unreadable") {
        return None;
    }
    let error = receipt
        .get("error")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("it names no error");
    Some(format!("the gate's receipt is unreadable: {error}"))
}

/// The equipment fault of a refusal: the checks the receipt marks
/// refused, and its own reason.
fn refused(receipt: Option<&Value>) -> FailureFacts {
    let said = |key: &str| {
        receipt
            .and_then(|r| r.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let checks = receipt
        .and_then(|r| r.get("checks"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| c.get("result").and_then(Value::as_str) == Some(crate::flake::REFUSED))
        .filter_map(|c| c.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    FailureFacts {
        equipment_fault: Some(EquipmentFault {
            kind: EquipmentFaultKind::Refused,
            checks,
            io_full_s: None,
            wall_s: None,
            why: format!(
                "the gate refused before it judged the branch: {}",
                quote(
                    said("refused_because")
                        .or_else(|| said("error"))
                        .unwrap_or("its receipt names no reason")
                )
            ),
        }),
        product_defect: None,
    }
}

/// PURE: a `failed` receipt read per failure entry. See the module
/// header for the rule; the conductor's `classify_red` reads a train
/// gate's receipt through this too.
pub fn read_failed_receipt(receipt: &Value) -> FailedReceipt {
    let mut out = FailedReceipt::default();
    let fails: Vec<&str> = receipt
        .get("fails")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let marker = fails.iter().position(|e| is_omission_marker(e));
    if let Some(cut) = fails_cut(&fails, marker) {
        out.defect(
            None,
            usize::from(marker.is_some()),
            format!(
                "the receipt's failure list is cut at its global bound ({cut}), so the omitted \
                 entries are unread and no timeout waives them"
            ),
        );
    }
    // Every entry but the omission marker, which stands for the unread.
    let read: Vec<&str> = fails
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != marker)
        .map(|(_, e)| *e)
        .collect();
    let mut failing: Vec<String> = receipt
        .get("checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        // Every check that did not pass, read the way `flake` reads it: a
        // result word this module does not know is a failure to evidence,
        // never a check to skip. A refusal judged nothing (bd4e8fb1).
        .filter(|c| {
            !matches!(
                c.get("result").and_then(Value::as_str),
                Some("pass") | Some(crate::flake::REFUSED)
            )
        })
        .filter_map(|c| c.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    for check in check_names(&read) {
        if !failing.contains(&check) {
            failing.push(check);
        }
    }
    for check in &failing {
        let entries: Vec<&str> = read
            .iter()
            .copied()
            .filter(|e| entry_check(e) == Some(check.as_str()))
            .filter(|e| !frames_every_line_it_counts(e))
            .collect();
        // Where a unittest entry's traceback is (backlog 05ddf1ca).
        let excerpt = receipt
            .get("fails_excerpt")
            .and_then(|x| x.get(check.as_str()))
            .and_then(Value::as_str);
        let Some((io_full_s, wall_s)) = check_stall(receipt, check) else {
            out.defect(
                Some(check),
                entries.len(),
                format!(
                    "{check} failed, and the receipt holds no measured io.full stall for its own \
                     interval"
                ),
            );
            continue;
        };
        if io_full_s < EQUIPMENT_IO_FULL_SECONDS {
            out.defect(
                Some(check),
                entries.len(),
                format!(
                    "{check} failed with {io_full_s} s of io.full stall in {wall_s} s, under the \
                     {EQUIPMENT_IO_FULL_SECONDS} s line"
                ),
            );
            continue;
        }
        if entries.is_empty() {
            out.defect(
                Some(check),
                0,
                format!(
                    "{check} failed, and the receipt records no failure entry for it to read a \
                     wait that ran out from"
                ),
            );
            continue;
        }
        let mut shapes: Vec<&str> = Vec::new();
        // The declared words that made each entry the drive's, so the
        // record says WHY it is equipment in the failure's own words.
        let mut phrases: Vec<&str> = Vec::new();
        let mut drives = 0;
        for entry in &entries {
            let not_the_drives = match read_entry(entry, excerpt) {
                EntryReading::Signed { shape, phrase } => {
                    drives += 1;
                    if !shapes.contains(&shape) {
                        shapes.push(shape);
                    }
                    if !phrases.contains(&phrase) {
                        phrases.push(phrase);
                    }
                    continue;
                }
                // The reason names what was read (backlog 724007d3): "no
                // timeout or deadline in this entry" was written about an
                // entry with the word deadline in it (gate 3082de78).
                EntryReading::Assertion => {
                    "on a Rust assertion in this entry, which is the branch's whatever it quotes"
                }
                EntryReading::NoMessage => "on an entry with no panic message to read",
                EntryReading::AssertionError => {
                    "on an AssertionError that ends this unittest entry's traceback, which is the \
                     branch's whatever it quotes"
                }
                EntryReading::NoTraceback => {
                    "on a unittest entry whose traceback the receipt's excerpt does not hold whole"
                }
                // Said of a traceback held whole, so neither reason may say
                // it is cut (backlog e27f7228).
                EntryReading::UnwalkedChain => {
                    "on a unittest traceback whose chain sentence does not follow one exception \
                     line, so it may join a quoted traceback's links as well as the test's own"
                }
                EntryReading::ForeignLink => {
                    "on a unittest traceback whose last chained link does not begin in the test's \
                     own function, so that link may be a quoted traceback's"
                }
                EntryReading::Unsigned => {
                    "on a panic message that matches no declared equipment signature (a wait that \
                     ran out)"
                }
            };
            out.defect(
                Some(check),
                1,
                format!(
                    "{check} failed {not_the_drives}, so no timeout beside it waives it: {}",
                    quote(entry)
                ),
            );
        }
        if shapes.is_empty() {
            continue;
        }
        let marker = shapes.join(", ");
        let said_as = phrases
            .iter()
            .map(|p| format!("\"{p}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let total = entries.len();
        out.said.push(if drives == total {
            format!(
                "{check} met a {marker} ({total} failure {}, every one equipment-shaped, said \
                 as {said_as}) with {io_full_s} s of io.full stall in {wall_s} s",
                if total == 1 { "entry" } else { "entries" }
            )
        } else {
            format!(
                "{check} met a {marker} in {drives} of its {total} failure entries (said as \
                 {said_as}) with {io_full_s} s of io.full stall in {wall_s} s"
            )
        });
        out.stalled.push(StalledCheck {
            check: check.clone(),
            io_full_s,
            wall_s,
            marker,
        });
    }
    // An entry that names no check cannot be inside a stalled one.
    for entry in read.iter().filter(|e| entry_check(e).is_none()) {
        out.defect(
            None,
            1,
            format!(
                "an entry names no check, so no check's stall can waive it: {}",
                quote(entry)
            ),
        );
    }
    // A red that names no failing check AND no error. The shape with an
    // `error` never reaches here: `failure_facts` and the conductor's
    // `classify_red` each take [`unjudged_because`] first. The defect is
    // the conductor's CHANGE reading, unchanged in car T3a; the recorder
    // reads `names_nothing` and writes `unjudged` equipment instead.
    if failing.is_empty() && out.defects.is_empty() {
        out.names_nothing = true;
        out.defect(None, 0, "the receipt names no failing check".into());
    }
    out
}

/// The check a `fails` entry names — the text before its first `:`
/// (`svelte-check: | Error: …`, `test: <name> - FAILED`), as the gate
/// runner writes every rung (infra/gate-runner/run.sh `detail`) — or None
/// for an entry with no `:` or an empty name.
fn entry_check(entry: &str) -> Option<&str> {
    let (check, _) = entry.split_once(':')?;
    let check = check.trim();
    (!check.is_empty()).then_some(check)
}

/// The checks a receipt's `fails` entries name ([`entry_check`]), in
/// first-seen order, once each. The conductor's judged-red reason reads
/// the same names (boss-cli `train/judged_red.rs` re-exports this).
pub fn gate_check_names(gate_fails: &[String]) -> Vec<String> {
    check_names(gate_fails)
}

fn check_names<S: AsRef<str>>(gate_fails: &[S]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in gate_fails {
        if let Some(check) = entry_check(line.as_ref())
            && !out.iter().any(|c| c == check)
        {
            out.push(check.to_string());
        }
    }
    out
}

/// Is this entry the runner's "+ N more entries omitted" marker, with or
/// without a check prefix (run.sh `write`)?
fn is_omission_marker(entry: &str) -> bool {
    let body = match entry.split_once(": ") {
        Some((_, rest)) if !entry.starts_with('+') => rest,
        _ => entry,
    };
    body.trim_start().starts_with('+')
        && (body.contains(" more entries omitted") || body.contains(" more entry omitted"))
}

/// Why the receipt's `fails` list is not whole, or None: the runner's
/// omission marker (at `marker`), or a list at [`RECEIPT_TOTAL_ENTRIES`]
/// (whose marker a clip could lose).
fn fails_cut(fails: &[&str], marker: Option<usize>) -> Option<String> {
    match marker {
        Some(i) => Some(format!(
            "{} entries, the last saying: {}",
            fails.len(),
            fails[i]
        )),
        None if fails.len() >= RECEIPT_TOTAL_ENTRIES => Some(format!(
            "{} entries, at the runner's cap of {RECEIPT_TOTAL_ENTRIES}",
            fails.len()
        )),
        None => None,
    }
}

/// What ONE `fails` entry's own panic message says about its cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryReading {
    /// It matches an [`EQUIPMENT_SIGNATURES`] phrase: the shape and the
    /// phrase it says first.
    Signed {
        shape: &'static str,
        phrase: &'static str,
    },
    /// A Rust assertion (`assertion …`): the branch's whatever it quotes.
    Assertion,
    /// No panic message to read: a roll-up of tests not named, a FAILED
    /// with no panic line, a check with no output.
    NoMessage,
    /// A panic message no declared phrase matches.
    Unsigned,
    /// A unittest entry whose traceback ends on an `AssertionError`: the
    /// Python form of [`EntryReading::Assertion`].
    AssertionError,
    /// A unittest entry whose traceback is not in the check's
    /// `fails_excerpt` whole: no excerpt, no block for the test, a block
    /// the excerpt's bound cut, or one with no exception after its frames.
    NoTraceback,
    /// A unittest traceback held whole, holding a chain sentence the
    /// forward walk did not take: a quoted traceback's own links and the
    /// test's after a link of several lines read alike (backlog
    /// 89ad2bf5). Its own reading since backlog e27f7228: it came back as
    /// [`EntryReading::NoTraceback`], whose reason the whole traceback
    /// beside it refuted.
    UnwalkedChain,
    /// A unittest traceback held whole whose last link reached by the
    /// walk does not open in the test's own function, so it may be a
    /// quote's (backlog 89ad2bf5; its own reading since e27f7228).
    ForeignLink,
}

/// PURE: ONE `fails` entry read by its own panic message — the text
/// after the runner's `panicked at <file>:<line>:<col>: `, never the
/// check or test name before it — or, for a unittest entry, by the
/// exception its traceback in `excerpt` ends on. Only
/// [`EntryReading::Signed`] can be the drive's; every other reading names
/// why it is not, and the product defect's `why` says that reason rather
/// than one an entry could refute.
fn read_entry(entry: &str, excerpt: Option<&str>) -> EntryReading {
    if let Some(header) = unittest_header(entry) {
        let exception = match excerpt.map(|x| unittest_exception(x, header)) {
            Some(Ok(exception)) => exception,
            Some(Err(refused)) => return refused,
            None => return EntryReading::NoTraceback,
        };
        let exception = exception.to_lowercase();
        if exception.starts_with("assertionerror") {
            return EntryReading::AssertionError;
        }
        return signed(&exception);
    }
    let Some(message) = entry
        .split_once("panicked at ")
        .and_then(|(_, after)| after.split_once(": "))
        .map(|(_, message)| message.trim_start().to_lowercase())
    else {
        return EntryReading::NoMessage;
    };
    if message.starts_with("assertion") {
        return EntryReading::Assertion;
    }
    signed(&message)
}

/// The `ERROR: <test> (<id>)` / `FAIL: <test> (<id>)` line a unittest
/// entry quotes — run.sh's error-line rung writes `<check>: | <line>` —
/// or None for every other entry.
fn unittest_header(entry: &str) -> Option<&str> {
    let (_, rest) = entry.split_once(':')?;
    let line = rest.trim_start().strip_prefix("| ")?.trim();
    (line.starts_with("ERROR: ") || line.starts_with("FAIL: ")).then_some(line)
}

/// The exception a unittest failure block ends on: the block opened by
/// `header` and unittest's dashed rule, up to the next `=`/`-` rule; in
/// it, the text after the frames of the last LINK of the traceback
/// unittest itself printed (a chained exception's is the one that failed
/// the test). Only the exception is returned, never a frame: gate
/// 35600cc4's frames quote `raise TimeoutExpired(` and `timeout=60` on
/// the way down, and a frame says what code ran, not what ran out.
///
/// unittest's traceback opens the block, and each further link of a
/// genuine chain is joined to the one before by a blank line, one of
/// Python's two chain sentences, and a blank line, walked forward from
/// that opening: frames, one exception line, the join. Any other
/// `Traceback` line sits inside an exception's message — a quoted
/// child's — and is never the test's: until backlog 1cacf298 the LAST one
/// anywhere was read, so a setUpClass AssertionError quoting a child's
/// `subprocess.TimeoutExpired` was waived as equipment. The exception
/// stops where such a quote begins.
///
/// The reading that refuses it — waiving nothing — when the evidence is
/// cut or cannot be told apart, each with its own reason (backlog
/// e27f7228): [`EntryReading::NoTraceback`] for a block that does not
/// open on unittest's own `Traceback`, a line of the excerpt's own `... (`
/// cut note inside it, or no exception after the frames;
/// [`EntryReading::UnwalkedChain`] for a chain sentence the walk did not
/// take (a quote's own links, or the test's after a link of several
/// lines); [`EntryReading::ForeignLink`] for a last link reached by the
/// walk whose first frame is not the test's own function (backlog
/// 89ad2bf5).
fn unittest_exception(excerpt: &str, header: &str) -> Result<String, EntryReading> {
    let cut = EntryReading::NoTraceback;
    const TRACEBACK: &str = "Traceback (most recent call last):";
    const CHAINED: [&str; 2] = [
        "During handling of the above exception, another exception occurred:",
        "The above exception was the direct cause of the following exception:",
    ];
    // unittest's separators: `'=' * 70` between failures, `'-' * 70`
    // under each header and above the `Ran N tests` summary.
    let rule = |line: &str, c: char| line.len() == 70 && line.chars().all(|x| x == c);
    let mut lines = excerpt
        .lines()
        .skip_while(|line| line.trim() != header)
        .skip(1);
    if !lines.next().is_some_and(|line| rule(line, '-')) {
        return Err(cut);
    }
    let block: Vec<&str> = lines
        .take_while(|line| !rule(line, '=') && !rule(line, '-'))
        .collect();
    if block.iter().any(|line| line.starts_with("... (")) {
        return Err(cut);
    }
    if block.first() != Some(&TRACEBACK) {
        return Err(cut);
    }
    // The chain is walked FORWARD from unittest's own Traceback: a link is
    // its frames and ONE exception line, and the next link is joined right
    // after that line. Until backlog 89ad2bf5 any Traceback with a join
    // before it was a link, so a quote BEGINNING mid-chain — a child's
    // stderr tail, or a cause never raised, which prints as one line and a
    // join — was followed to the child's TimeoutExpired.
    let mut link = 0;
    let at = loop {
        let at = (link + 1..block.len())
            .find(|&i| !block[i].starts_with(' '))
            .ok_or(cut)?;
        match block.get(at + 1..at + 5) {
            Some(["", sentence, "", next]) if CHAINED.contains(sentence) && *next == TRACEBACK => {
                link = at + 4;
            }
            _ => break at,
        }
    };
    // A chain sentence the walk did not take joins either a quote's links or
    // the test's own after a link of several lines; the two read alike, so
    // the block waives nothing.
    if block[at..].iter().any(|line| CHAINED.contains(line)) {
        return Err(EntryReading::UnwalkedChain);
    }
    // A link the walk did reach must still be the test's: the exception
    // that escaped it is printed from the test's own function down, while
    // a quote that shares the assertion's line runs from the child's.
    if link > 0
        && !header
            .split_whitespace()
            .nth(1)
            .zip(block.get(link + 1))
            .is_some_and(|(test, frame)| frame.ends_with(&format!(", in {test}")))
    {
        return Err(EntryReading::ForeignLink);
    }
    let exception = block[at..]
        .iter()
        .take_while(|line| **line != TRACEBACK)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let exception = exception.trim();
    if exception.is_empty() {
        return Err(cut);
    }
    Ok(exception.to_string())
}

/// Is this entry run.sh's error-line framing line — `<check>: no cargo
/// test failure in this check's output; N error line(s), first M:` —
/// quoting every line it counts (M == N)? Then it says nothing the
/// quoted entries do not, and is not an entry of its own. Counting more
/// than it quotes, it stands for the unquoted lines, and is read as an
/// entry with no message: the product's.
fn frames_every_line_it_counts(entry: &str) -> bool {
    let Some((_, rest)) = entry.split_once(':') else {
        return false;
    };
    let Some((counted, quoted)) = rest
        .trim_start()
        .strip_prefix("no cargo test failure in this check's output; ")
        .and_then(|counts| counts.strip_suffix(':'))
        .and_then(|counts| counts.split_once(" error line(s), first "))
    else {
        return false;
    };
    matches!(
        (counted.parse::<usize>(), quoted.parse::<usize>()),
        (Ok(n), Ok(m)) if n == m
    )
}

/// A message read against [`EQUIPMENT_SIGNATURES`]: the phrase it says
/// FIRST, so the record names the failure's own words.
fn signed(message: &str) -> EntryReading {
    EQUIPMENT_SIGNATURES
        .iter()
        .flat_map(|sig| sig.phrases.iter().map(move |p| (*p, sig.shape)))
        .filter_map(|(phrase, shape)| message.find(phrase).map(|at| (at, phrase, shape)))
        .min()
        .map_or(EntryReading::Unsigned, |(_, phrase, shape)| {
            EntryReading::Signed { shape, phrase }
        })
}

/// `(io.full stall seconds, wall seconds)` of the check's own measured
/// interval — `check-start:<check>` to `check-finish:<check>` in the
/// receipt's `runtime_evidence` — or None when the receipt holds no such
/// interval or did not measure io.full in it.
fn check_stall(receipt: &Value, check: &str) -> Option<(u64, u64)> {
    let (from, to) = (
        format!("check-start:{check}"),
        format!("check-finish:{check}"),
    );
    receipt
        .pointer("/runtime_evidence/intervals")
        .and_then(Value::as_array)?
        .iter()
        .find(|i| {
            i.get("from").and_then(Value::as_str) == Some(from.as_str())
                && i.get("to").and_then(Value::as_str) == Some(to.as_str())
                && i.get("state").and_then(Value::as_str) == Some("measured")
        })
        .and_then(|i| {
            let io_full = i.pointer("/stall_usec/io.full")?.as_u64()?;
            let wall = i.get("elapsed_ns").and_then(Value::as_u64).unwrap_or(0);
            Some((io_full / 1_000_000, wall / 1_000_000_000))
        })
}

/// At most [`WHY_QUOTE_CHARS`] characters of the receipt's own words,
/// said to be cut when they are.
fn quote(text: &str) -> String {
    match text.char_indices().nth(WHY_QUOTE_CHARS) {
        None => text.to_string(),
        Some((at, _)) => format!(
            "{} … [cut; the receipt beside this has it whole]",
            &text[..at]
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Train 2258c95d's gate-run 0de50487 receipt, trimmed (the fixture
    /// 2e9a2449's classifier tests read): one failing `test` check, its
    /// deadline entry as the runner wrote it, and its own interval as
    /// `runtime_evidence` measured it (1710 s of wall).
    fn receipt_0de50487(io_full_usec: u64) -> Value {
        json!({
            "verdict": "failed",
            "mode": "auto",
            "head": "d60f4654397da2783e06798568cbf1d784a1dc4f",
            "checks": [
                {"name": "clippy", "result": "pass", "seconds": 138},
                {"name": "test", "result": "fail", "seconds": 1711},
            ],
            "fails": ["test: gcp_root_tree_sh::authenticated_network_main_preserves_history_and_old_generation_on_failure - panicked at crates/core/boss-testing/tests/groups/../gcp_root_tree_sh.rs:325:37: HTTP fixture did not start: port publication deadline expired; last read: Ok(\"33587\"); elapsed=4.754238073s; timeout=1s"],
            "runtime_evidence": {"intervals": [
                {"from": "check-start:clippy", "to": "check-finish:clippy", "state": "measured",
                 "elapsed_ns": 138_000_000_000_u64, "stall_usec": {"io.full": 19_000_000}},
                {"from": "check-start:test", "to": "check-finish:test", "state": "measured",
                 "elapsed_ns": 1_710_000_000_000_u64, "stall_usec": {"io.full": io_full_usec}},
            ]}
        })
    }

    const CONSERVATION: &str = "test: ledger::conservation - panicked at crates/core/boss-testing/tests/conservation.rs:12:3: assertion left == right failed: left 1 right 2";

    // -- the readers' half (car T3b) ---------------------------------------
    //
    // What the writer records is what every reader reads back: the step
    // metadata a `record-verdict` completion leaves, round-tripped.

    /// The `record-verdict` metadata the conductor's recorder writes.
    fn step_md(verdict: &str, receipt: &Value) -> Value {
        let mut md = json!({"verdict": verdict, "receipt": receipt.to_string()});
        for (k, v) in failure_facts(verdict, Some(receipt)).fields() {
            md[k] = v;
        }
        md
    }

    // -- the v2 name (station tree T3, backlog 490c12f0) ---------------------
    //
    // v2 calls the fault `machine_fault`. This build reads it under either
    // name and still writes `equipment_fault`, the name the last-converged
    // build reads; the retirement car flips the writer.

    /// The recorder's metadata with its fault moved to the v2 key, as the
    /// retirement car's writer will leave it.
    fn as_v2(mut md: Value) -> Value {
        let fault = md
            .as_object_mut()
            .and_then(|m| m.remove(EQUIPMENT_FAULT))
            .expect("the record carries a fault to rename");
        md["machine_fault"] = fault;
        md
    }

    #[test]
    fn a_fault_recorded_under_its_v2_name_reads_as_the_machine_fault() {
        let stall = receipt_0de50487(651_000_000);
        let v1 = recorded(&step_md("failed", &stall)).expect("recorded");
        let v2 = recorded(&as_v2(step_md("failed", &stall))).expect("recorded under v2");
        assert_eq!(v2, v1, "the same fact under either name");
        assert_eq!(
            v2.releasing_fault().map(|f| f.kind),
            Some(EquipmentFaultKind::StallDeadline)
        );
        // Both names, saying the same thing, are one fact.
        let mut both = step_md("failed", &stall);
        both["machine_fault"] = both[EQUIPMENT_FAULT].clone();
        assert_eq!(recorded(&both), Some(v1));
    }

    /// Two names that disagree are not a fault anyone may act on: the
    /// record cannot say which one happened, so it judged nothing, and
    /// it is never read as clean.
    #[test]
    fn two_names_for_the_fault_that_disagree_read_as_unjudged() {
        let mut md = step_md("failed", &receipt_0de50487(651_000_000));
        md["machine_fault"] = json!({"kind": "lost", "checks": [], "why": "the pod ended"});
        let facts = recorded(&md).expect("recorded");
        let unjudged = facts.unjudged().expect("disagreeing names judge nothing");
        assert!(unjudged.why.contains("disagree"), "{}", unjudged.why);
        assert_eq!(facts.releasing_fault(), None);
    }

    #[test]
    fn a_record_with_neither_field_is_unrecorded_never_clean() {
        // A gate-run recorded before car T3a, and a green: neither key.
        let old =
            json!({"verdict": "failed", "receipt": receipt_0de50487(651_000_000).to_string()});
        assert_eq!(recorded(&old), None);
        assert_eq!(recorded(&json!({"verdict": "green"})), None);
        let nulls = json!({"verdict": "failed", EQUIPMENT_FAULT: null, PRODUCT_DEFECT: null});
        assert_eq!(recorded(&nulls), None);
    }

    #[test]
    fn the_reader_reads_back_what_the_writer_recorded() {
        let stall = receipt_0de50487(651_000_000);
        let facts = recorded(&step_md("failed", &stall)).expect("recorded");
        assert_eq!(facts, failure_facts("failed", Some(&stall)));
        let f = facts.releasing_fault().expect("stall-deadline releases");
        assert_eq!(f.kind, EquipmentFaultKind::StallDeadline);
        assert!(!facts.strikes());
        assert_eq!(facts.unjudged(), None);
        assert_eq!(facts.names(), "an equipment fault (stall-deadline)");
    }

    #[test]
    fn a_product_defect_strikes_alone_or_beside_an_equipment_fault() {
        let product = recorded(&step_md("failed", &receipt_0de50487(59_000_000))).unwrap();
        assert!(product.strikes());
        assert_eq!(product.releasing_fault(), None);
        assert_eq!(product.names(), "a product defect");

        let mut mixed = receipt_0de50487(651_000_000);
        mixed["fails"]
            .as_array_mut()
            .unwrap()
            .push(json!(CONSERVATION));
        let both = recorded(&step_md("failed", &mixed)).unwrap();
        assert!(both.equipment_fault.is_some() && both.strikes());
        assert_eq!(both.releasing_fault(), None, "the product defect governs");
        assert_eq!(
            both.names(),
            "a product defect and an equipment fault (stall-deadline)"
        );
    }

    #[test]
    fn an_unjudged_record_neither_strikes_nor_releases() {
        let unread = json!({"verdict": "unreadable", "error": "no receipt.json"});
        let facts = recorded(&step_md("failed", &unread)).unwrap();
        assert!(!facts.strikes());
        assert_eq!(facts.releasing_fault(), None);
        assert!(
            facts
                .unjudged()
                .is_some_and(|f| f.why.contains("no receipt.json")),
            "{facts:?}"
        );
        assert_eq!(facts.names(), "an equipment fault (unjudged)");
    }

    #[test]
    fn a_present_field_that_will_not_read_is_still_a_fact() {
        let md = json!({"verdict": "failed", PRODUCT_DEFECT: "a string, not an object"});
        let facts = recorded(&md).unwrap();
        assert!(facts.strikes(), "a written product defect strikes");
        let md = json!({"verdict": "failed", EQUIPMENT_FAULT: {"kind": "a-new-kind"}});
        let facts = recorded(&md).unwrap();
        assert!(
            facts
                .unjudged()
                .is_some_and(|f| f.why.contains("could not be read")),
            "an unreadable equipment fault releases nobody: {facts:?}"
        );
    }

    /// Moved from boss-cli `train/equipment_red.rs` with the constant it
    /// pins (c02fc3b3): the pin lives beside the one definition.
    #[test]
    fn the_receipt_bound_is_the_runners() {
        let path = boss_testing::repo_root().join("infra/gate-runner/run.sh");
        let run = std::fs::read_to_string(&path).expect("infra/gate-runner/run.sh");
        let declared: Vec<usize> = run
            .lines()
            .filter_map(|l| l.strip_prefix("TOTAL_ENTRIES = "))
            .filter_map(|v| v.split_whitespace().next()?.parse().ok())
            .collect();
        assert_eq!(
            declared,
            vec![RECEIPT_TOTAL_ENTRIES],
            "run.sh TOTAL_ENTRIES moved: RECEIPT_TOTAL_ENTRIES must follow it"
        );
        assert!(
            run.contains("more entr%s omitted"),
            "run.sh no longer writes the omission marker is_omission_marker reads"
        );
    }

    #[test]
    fn a_refused_gate_records_an_equipment_fault_and_no_product_defect() {
        let r = json!({
            "verdict": "refused",
            "refused_because": "disk floor: 61GB free, the floor is 80GB",
            "checks": [{"name": "disk-floor", "result": "refused"}],
        });
        let f = failure_facts("refused", Some(&r));
        let eq = f.equipment_fault.expect("a refusal is the equipment's");
        assert_eq!(eq.kind, EquipmentFaultKind::Refused);
        assert_eq!(eq.checks, vec!["disk-floor".to_string()]);
        assert!(eq.why.contains("disk floor: 61GB free"), "{}", eq.why);
        assert_eq!(
            f.product_defect, None,
            "a refusal judged nothing about the branch"
        );
    }

    #[test]
    fn a_lost_gate_records_an_equipment_fault_and_no_product_defect() {
        // A lost receipt is the conductor's prose, not JSON.
        let f = failure_facts("lost", None);
        let eq = f.equipment_fault.expect("a lost run is the equipment's");
        assert_eq!(eq.kind, EquipmentFaultKind::Lost);
        assert_eq!(eq.io_full_s, None, "nothing measured a stall");
        assert_eq!(f.product_defect, None);
    }

    #[test]
    fn a_failed_gate_whose_only_entries_are_deadlines_over_the_stall_line_is_equipment() {
        let f = failure_facts("failed", Some(&receipt_0de50487(651_000_000)));
        let eq = f
            .equipment_fault
            .expect("a deadline over the line is the drive's");
        assert_eq!(eq.kind, EquipmentFaultKind::StallDeadline);
        assert_eq!(eq.checks, vec!["test".to_string()]);
        assert_eq!(eq.io_full_s, Some(651));
        assert_eq!(eq.wall_s, Some(1710));
        assert!(eq.why.contains("651 s of io.full"), "{}", eq.why);
        assert_eq!(f.product_defect, None);
    }

    #[test]
    fn the_same_deadline_under_the_stall_line_is_a_product_defect() {
        let f = failure_facts("failed", Some(&receipt_0de50487(59_000_000)));
        assert_eq!(f.equipment_fault, None);
        let pd = f.product_defect.expect("under the line, the branch's");
        assert_eq!(pd.checks, vec!["test".to_string()]);
        assert_eq!(pd.entries, 1);
        assert!(
            pd.why.contains("59 s") && pd.why.contains("under"),
            "{}",
            pd.why
        );
    }

    #[test]
    fn an_assertion_beside_a_deadline_records_both_facts() {
        let mut r = receipt_0de50487(651_000_000);
        r["fails"].as_array_mut().unwrap().push(json!(CONSERVATION));
        let f = failure_facts("failed", Some(&r));
        let eq = f
            .equipment_fault
            .expect("the deadline entry is the drive's");
        assert_eq!(eq.kind, EquipmentFaultKind::StallDeadline);
        assert_eq!(eq.checks, vec!["test".to_string()]);
        assert!(eq.why.contains("1 of its 2 failure entries"), "{}", eq.why);
        let pd = f.product_defect.expect("the assertion is the branch's");
        assert_eq!(pd.checks, vec!["test".to_string()]);
        assert_eq!(pd.entries, 1);
        assert!(pd.why.contains("ledger::conservation"), "{}", pd.why);
    }

    /// The runner's global omission marker (infra/gate-runner/run.sh
    /// `write`) for `dropped` entries.
    fn omitted(dropped: usize) -> String {
        format!(
            "+ {dropped} more entr{} omitted - this receipt caps `fails` at 40 so it stays \
             readable; the Job log replay has the rest",
            if dropped == 1 { "y" } else { "ies" }
        )
    }

    #[test]
    fn a_cut_failure_list_records_a_product_defect() {
        let mut r = receipt_0de50487(651_000_000);
        r["fails"].as_array_mut().unwrap().push(json!(omitted(3)));
        let f = failure_facts("failed", Some(&r));
        let pd = f.product_defect.expect("the omitted entries are unread");
        assert!(pd.why.contains("omitted"), "{}", pd.why);
        assert_eq!(pd.entries, 1, "the marker stands for the unread entries");
        // The entry that WAS read is still the drive's, and says so.
        let eq = f
            .equipment_fault
            .expect("the visible deadline is a fact too");
        assert_eq!(eq.checks, vec!["test".to_string()]);
    }

    #[test]
    fn a_list_at_the_runners_cap_records_a_product_defect_without_its_marker() {
        let mut r = receipt_0de50487(651_000_000);
        let entry = r["fails"][0].clone();
        r["fails"] = json!(vec![entry; RECEIPT_TOTAL_ENTRIES]);
        let f = failure_facts("failed", Some(&r));
        let pd = f.product_defect.expect("a list at the cap is never whole");
        assert!(pd.why.contains("40"), "{}", pd.why);
    }

    #[test]
    fn a_green_gate_records_neither_fact() {
        let r = json!({"verdict": "green", "head": "d60f4654397da2783e06798568cbf1d784a1dc4f"});
        assert_eq!(failure_facts("green", Some(&r)), FailureFacts::default());
        assert_eq!(failure_facts("withdrawn", None), FailureFacts::default());
    }

    /// Train ed012255's train gate at e48609e2 (worked_example_train_ed012255
    /// on agent-run 5a73674a, triage of alarm 593ad17c): one `test` check
    /// over the stall line holding three entries — an assertion-shaped
    /// isolated child (the TLS observer), a broker fixture that timed out,
    /// and a membership projection reported stale. The TLS child matches
    /// no declared signature, so the branch is not given the benefit:
    /// product_defect, with the timeout recorded beside it as the drive's.
    /// The stale projection was a second product entry until backlog
    /// 724007d3 declared its shape: "stale" there is a 5 s wall-clock
    /// freshness window (`HEALTHY_FOR`, in membership_projection_service.rs
    /// until design 7b8ed079 car 2 retired the membership replay) that ran
    /// out, which is the drive's when the check stalled past the line, as
    /// gate a71c29c3 showed it at 160 s.
    #[test]
    fn train_ed012255_records_a_product_defect_for_its_unsigned_entry() {
        let mut r = receipt_0de50487(366_000_000);
        r["fails"] = json!([
            "test: observer::tests::inherited_keylogging_ambient_ca_and_proxy_settings_cannot_change_the_tls_observer - panicked at crates/core/boss-dispatcher-handlers/src/observer/tests.rs:144:5: isolated synthetic child failed; child: assertion `left == right` failed\n  left: false\n right: true",
            "test: credentials::broker_stage::tests::the_pod_token_never_crosses_a_connection_that_did_not_verify - panicked at crates/core/boss-jobs/src/credentials/broker_stage.rs:880:9: fixture read failed: Resource temporarily unavailable (os error 11) ... operation timed out",
            "test: membership_history_pg::tests::an_owned_native_bootstrap_projects_each_seed_once - panicked at crates/core/boss-jobs/src/membership_history_pg/tests.rs:2210:14: membership projection is stale; last generation 02:30:57",
        ]);
        let f = failure_facts("failed", Some(&r));
        let pd = f
            .product_defect
            .expect("the TLS child matches no declared signature");
        assert_eq!(pd.entries, 1);
        assert_eq!(pd.checks, vec!["test".to_string()]);
        assert!(pd.why.contains("tls_observer"), "{}", pd.why);
        let eq = f
            .equipment_fault
            .expect("the broker timeout is the drive's");
        assert!(eq.why.contains("timeout"), "{}", eq.why);
        assert!(eq.why.contains("2 of its 3 failure entries"), "{}", eq.why);
    }

    // -- the deadline vocabulary (backlog 724007d3) ----------------------
    //
    // Four train and car gates on 2026-11-04/05 failed under an io.full
    // stall far past the line, and each was recorded product_defect
    // because its entries said their deadline in words the table did not
    // declare. The receipts below are the recorded ones, the `fails`
    // entries copied verbatim and the `test` check's own interval as
    // `runtime_evidence` measured it.

    /// A receipt with one failing `test` check whose own measured interval
    /// stalled `io_full_usec` in `elapsed_ns`, holding `fails`.
    fn stalled_test(io_full_usec: u64, elapsed_ns: u64, fails: &[&str]) -> Value {
        let mut r = receipt_0de50487(io_full_usec);
        r["fails"] = json!(fails);
        r["runtime_evidence"]["intervals"][1]["elapsed_ns"] = json!(elapsed_ns);
        r
    }

    /// Train 23e08ca0's gate 1a55a341 (2026-11-04 23:50Z, 178 s of io.full
    /// in 1235 s): the boss-step verb said its own bound ran out — "not
    /// answered within 2s (curl exit 28; BOSS_STEP_MAX_TIME)" — against the
    /// test's local fixture. Recorded product_defect "on no timeout or
    /// deadline", and console car 4 (bbaa087b), which does not touch the
    /// test, was struck for it.
    const GATE_1A55A341: &str = "test: a_step_close_has_a_deadline::the_metadata_merge_has_a_deadline - panicked at crates/core/boss-testing/tests/groups/../a_step_close_has_a_deadline.rs:183:5: boss-step: jobs-api unreachable at http://127.0.0.1:36537, not answered within 2s (curl exit 28; BOSS_STEP_MAX_TIME) — 'run' not recorded";

    #[test]
    fn gate_1a55a341_a_verb_not_answered_within_its_bound_is_equipment() {
        let r = stalled_test(178_446_160, 1_235_652_325_977, &[GATE_1A55A341]);
        let f = failure_facts("failed", Some(&r));
        assert_eq!(f.product_defect, None, "{f:?}");
        let eq = f.equipment_fault.expect("the verb's own deadline ran out");
        assert_eq!(eq.kind, EquipmentFaultKind::StallDeadline);
        assert_eq!(eq.checks, vec!["test".to_string()]);
        assert_eq!((eq.io_full_s, eq.wall_s), (Some(178), Some(1235)));
        assert!(
            eq.why.contains("\"not answered within\""),
            "the why names the words that made it the drive's: {}",
            eq.why
        );
    }

    /// Car gate b47c77d8 (174 s of io.full in 1333 s): the pg_locks wait
    /// loop's `tokio::time::timeout(..).await.unwrap()` panicked on tokio's
    /// `Elapsed`, whose Debug form is `Elapsed(())` — the Display form,
    /// "deadline has elapsed", was declared and never printed by an unwrap.
    #[test]
    fn gate_b47c77d8_a_tokio_elapsed_unwrapped_is_equipment() {
        let r = stalled_test(
            174_579_443,
            1_333_208_714_478,
            &[
                "test: native_selection_store::tests::real_policy_port_retirement_waits_for_native_transaction_then_records_its_audit - panicked at crates/core/boss-jobs/src/native_selection_store.rs:4646:18: called `Result::unwrap()` on an `Err` value: Elapsed(())",
            ],
        );
        let f = failure_facts("failed", Some(&r));
        assert_eq!(f.product_defect, None, "{f:?}");
        let eq = f.equipment_fault.expect("tokio's deadline elapsed");
        assert_eq!((eq.io_full_s, eq.wall_s), (Some(174), Some(1333)));
        assert!(eq.why.contains("\"elapsed(())\""), "{}", eq.why);
    }

    /// Train 9db64a74's gate a71c29c3 (2026-11-05 02:45Z, 160 s of io.full
    /// in 1344 s): three membership_history_pg entries, recorded
    /// product_defect {entries: 3}. Not the first entry judged and applied
    /// to all, as the conductor's note of 02:55Z read it: the `why` quotes
    /// the FIRST product reason and counts the rest, and all three were
    /// read as the product's — two because "deadline exhausted" (BOSS's
    /// own spelling across the membership, claims and history readers) was
    /// not declared, one because a projection gone stale past its 5 s
    /// wall-clock freshness window (`HEALTHY_FOR`) was not.
    #[test]
    fn gate_a71c29c3_an_exhausted_deadline_and_a_stale_projection_are_equipment() {
        let r = stalled_test(
            160_691_963,
            1_344_902_546_103,
            &[
                "test: membership_history_pg::tests::an_owned_native_bootstrap_projects_each_seed_once - panicked at crates/core/boss-jobs/src/membership_history_pg/tests.rs:2407:60: called `Result::unwrap()` on an `Err` value: \"membership projection is stale; last generation 2026-11-05 02:19:06.146877 UTC\"",
                "test: membership_history_pg::tests::original_source_budget_includes_waiting_for_a_pool_connection - panicked at crates/core/boss-jobs/src/membership_history_pg/tests.rs:546:5: native membership replay deadline exhausted",
                "test: membership_history_pg::tests::starting_state_observation_preserves_partial_plans_and_owns_timeout_cleanup - panicked at crates/core/boss-jobs/src/membership_history_pg/tests.rs:993:5: Observation { state: \"unavailable\", why: Some(\"native membership replay deadline exhausted\"), since: Some(2026-11-05T02:19:58.376266Z), until: None, audit_cutoff: None, window_cut: None, carried_packets: 0, exc... (+197 char(s); the Job log replay has the full text)",
            ],
        );
        let f = failure_facts("failed", Some(&r));
        assert_eq!(f.product_defect, None, "{f:?}");
        let eq = f
            .equipment_fault
            .expect("every entry is a wait that ran out");
        assert!(
            eq.why
                .contains("3 failure entries, every one equipment-shaped"),
            "{}",
            eq.why
        );
        assert!(
            eq.why.contains("\"deadline exhausted\"") && eq.why.contains("\"projection is stale\""),
            "{}",
            eq.why
        );
        assert_eq!(eq.io_full_s, Some(160));
    }

    /// Train 9b5c1889's gate 3082de78 (2026-11-05 06:17Z, 280 s of io.full
    /// in 1282 s): two entries, recorded product_defect {entries: 2}. The
    /// boss-step entry is the 1a55a341 shape and is the drive's. The other
    /// is the test's OWN assertion message — "the record names the deadline
    /// and how far it got" states what the test expected, not that a wait
    /// ran out — so it stays the product's, and the stall does not waive
    /// it: a stall over the line makes a wait that ran out the drive's, it
    /// never makes an assertion the drive's (a_stalled_check_that_failed_
    /// on_an_assertion_is_change, review f7f21501). What changes is that
    /// the record now carries the equipment fault beside it, and a `why`
    /// that says what it checked instead of "no timeout or deadline in
    /// this entry" about an entry with the word deadline in it.
    #[test]
    fn gate_3082de78_records_the_verbs_deadline_beside_the_tests_own_assertion() {
        let r = stalled_test(
            280_734_248,
            1_282_926_954_515,
            &[
                "test: every_converge_tick_observes_the_site::the_page_phase_stops_at_the_observation_deadline_and_says_so - panicked at crates/core/boss-testing/tests/groups/../every_converge_tick_observes_the_site.rs:741:5: the record names the deadline and how far it got: {\"site\":{\"bytes\":50,\"hash\":\"0d1d9d81d46ddf169b3b3a77f9aed050f47b20eb4ea48fdc5c8ef31f9cfa9aae\",\"host\":\"www.algedonic.dev\",\"http_status\":200,\"n... (+291 char(s); the Job log replay has the full text)",
                "test: a_step_close_has_a_deadline::the_metadata_merge_has_a_deadline - panicked at crates/core/boss-testing/tests/groups/../a_step_close_has_a_deadline.rs:215:5: boss-step: jobs-api unreachable at http://127.0.0.1:38121, not answered within 10s (curl exit 28; BOSS_STEP_MAX_TIME) — 'run' not recorded",
            ],
        );
        let f = failure_facts("failed", Some(&r));
        let eq = f
            .equipment_fault
            .expect("the boss-step entry is the drive's");
        assert!(eq.why.contains("1 of its 2 failure entries"), "{}", eq.why);
        let pd = f.product_defect.expect("the test's own assertion stays");
        assert_eq!(pd.entries, 1);
        assert!(
            pd.why
                .contains("the_page_phase_stops_at_the_observation_deadline"),
            "{}",
            pd.why
        );
        assert!(
            !pd.why.contains("no timeout or deadline in this entry"),
            "the why says what it checked, not a claim the entry refutes: {}",
            pd.why
        );
        assert!(
            pd.why.contains("no declared equipment signature"),
            "{}",
            pd.why
        );
    }

    /// Train 260dbd83's gate d7b85890 (2026-11-05 18:5xZ, 253 s of io.full
    /// in 637 s, two bays busy): the claims-history adapter's pre-commit
    /// statement budget — 2 s, monotonic, spent across every statement of
    /// one operation (boss-events `statement_budget.rs`) — ran out inside
    /// `rebuild_native`. Recorded product_defect "on a panic message that
    /// matches no declared equipment signature", so the red rode as the
    /// branch's while car f7ac5877 touched nothing the test reads (backlog
    /// 21217d8a; the same mechanism as 97a5a57d's envelope test). The
    /// entry below is the receipt's, verbatim.
    const GATE_D7B85890: &str = "test: lease_history_completed_stock_remains_charged_until_verified_return - panicked at crates/core/boss-jobs/tests/flow_resource_claims.rs:1416:10: called `Result::unwrap()` on an `Err` value: Unavailable(\"pre-commit statement budget expired\")";
    #[test]
    fn gate_d7b85890_a_spent_pre_commit_statement_budget_is_equipment() {
        let r = stalled_test(253_194_084, 637_794_884_550, &[GATE_D7B85890]);
        let f = failure_facts("failed", Some(&r));
        assert_eq!(f.product_defect, None, "{f:?}");
        let eq = f.equipment_fault.expect("the statement budget ran out");
        assert_eq!(eq.kind, EquipmentFaultKind::StallDeadline);
        assert_eq!(eq.checks, vec!["test".to_string()]);
        assert_eq!((eq.io_full_s, eq.wall_s), (Some(253), Some(637)));
        assert!(
            eq.why.contains("\"pre-commit statement budget expired\""),
            "the alert names the budget in the failure's own words: {}",
            eq.why
        );
        // Under the line the branch is not given the benefit.
        let under = failure_facts(
            "failed",
            Some(&stalled_test(59_000_000, 1, &[GATE_D7B85890])),
        );
        assert_eq!(under.equipment_fault, None, "{under:?}");
        assert!(under.product_defect.is_some(), "{under:?}");
        // A plain assertion that quotes the budget stays the branch's, stall
        // or no stall: the phrase makes a wait that ran out the drive's, it
        // never makes an assertion the drive's.
        let assertion = "test: claims::budget_names_itself - panicked at crates/core/boss-jobs/tests/c.rs:1:1: assertion `left == right` failed: pre-commit statement budget expired\n  left: 1\n right: 2";
        let f = failure_facts("failed", Some(&stalled_test(253_194_084, 1, &[assertion])));
        assert_eq!(f.equipment_fault, None, "{f:?}");
        assert!(
            f.product_defect
                .is_some_and(|pd| pd.why.contains("on a Rust assertion")),
            "an assertion stays CHANGE"
        );
    }

    /// The rest of the vocabulary the item names, one entry each: curl's
    /// exit 28 (operation timed out) as a verb reports it, timeout(1)'s
    /// exit 124, and an observer socket deadline as OpenSSL displays it —
    /// `HandshakeError::WouldBlock` around a WANT_READ that carries an io
    /// error (openssl 0.10 ssl/error.rs), the TLS seam builder's (run
    /// 83f19197) signature. Each stays under its check's stall line in the
    /// control, where the branch is not given the benefit.
    #[test]
    fn the_declared_vocabulary_reads_each_deadline_in_its_own_words() {
        for (entry, phrase) in [
            (
                "test: estate::observe_units_post - panicked at crates/core/boss-testing/tests/o.rs:1:1: observe-units.sh: the POST failed (curl exit 28), failing the unit",
                "curl exit 28",
            ),
            (
                "test: judged::verb_bound - panicked at crates/core/boss-testing/tests/j.rs:1:1: boss-step: killed at 30 s — exit 124 — before its verdict line",
                "exit 124",
            ),
            (
                "test: observer::tests::observes_the_site_over_tls - panicked at crates/core/boss-dispatcher-handlers/src/observer/tests.rs:1:1: observer read failed: the handshake was interrupted: a nonblocking read call would have blocked",
                "the handshake was interrupted: a nonblocking read call would have blocked",
            ),
        ] {
            let f = failure_facts("failed", Some(&stalled_test(651_000_000, 1, &[entry])));
            assert_eq!(f.product_defect, None, "{entry}: {f:?}");
            let eq = f
                .equipment_fault
                .unwrap_or_else(|| panic!("{entry}: the drive's"));
            assert!(eq.why.contains(&format!("\"{phrase}\"")), "{}", eq.why);
            let under = failure_facts("failed", Some(&stalled_test(59_000_000, 1, &[entry])));
            assert_eq!(under.equipment_fault, None, "{entry}: under the line");
            assert!(under.product_defect.is_some(), "{entry}");
        }
    }

    // -- a Python unittest entry (backlog 05ddf1ca) ------------------------
    //
    // The lint runners are Python unittest suites, and run.sh's error-line
    // rung writes their failure as a framing line plus `| ERROR: <test>
    // (<id>)` — no `panicked at`, so until this both entries read as "no
    // panic message" and the stall beside them could never waive either.
    // The traceback is on the same receipt, in `fails_excerpt[<check>]`.
    // The entries and the excerpt below are gate 35600cc4's (2026-11-05
    // 20:51Z), verbatim; that receipt measured no interval for the check
    // (121 intervals omitted), so the 101 s stall is the review's
    // (e774b3e8) synthetic one, and the as-recorded shape is `None`.

    const GATE_35600CC4_CHECK: &str = "the-converge-watches-postgres";
    const GATE_35600CC4_FAILS: [&str; 2] = [
        "the-converge-watches-postgres: no cargo test failure in this check's output; 1 error line(s), first 1:",
        "the-converge-watches-postgres: | ERROR: test_named_target_resolves_real_commit_and_renders_its_own_registry (__main__.RecoveryContracts.test_named_target_resolves_real_commit_and_renders_its_own_registry)",
    ];
    const GATE_35600CC4_EXCERPT: &str = "{\"owned_child_exit\": 91, \"stdout\": \"owned child exits after durable attempt marker, before PostgreSQL write\\n\", \"stderr\": \"\"}\nloopback transport client version: {\n  \"clientVersion\": {\n    \"major\": \"1\",\n    \"minor\": \"33\",\n    \"gitVersion\": \"v1.33.3\",\n    \"gitCommit\": \"80779bd6ff08b451e1c165a338a7b69351e9b0b8\",\n    \"gitTreeState\": \"clean\",\n    \"buildDate\": \"2025-07-15T18:06:34Z\",\n    \"goVersion\": \"go1.24.4\",\n    \"compiler\": \"gc\",\n    \"platform\": \"linux/amd64\"\n  },\n  \"kustomizeVersion\": \"v5.6.0\"\n}\n\n{\"actual_delete_uri\": \"/api/v1/namespaces/boss/pods/postgres-0?timeout=5s\", \"actual_delete_body_base64\": \"eyJraW5kIjogIkRlbGV0ZU9wdGlvbnMiLCAiYXBpVmVyc2lvbiI6ICJ2MSIsICJncmFjZVBlcmlvZFNlY29uZHMiOiAzMCwgInByZWNvbmRpdGlvbnMiOiB7InVpZCI6ICJleGFjdC1mYWlsZWQtcG9kIn19\"}\nactual kubectl loopback DELETE captured exact UID and graceful30s DeleteOptions\n......................E....................\n======================================================================\nERROR: test_named_target_resolves_real_commit_and_renders_its_own_registry (__main__.RecoveryContracts.test_named_target_resolves_real_commit_and_renders_its_own_registry)\n----------------------------------------------------------------------\nTraceback (most recent call last):\n  File \"/gate-target/repo/infra/forge/tests/postgres-rollout-test.py\", line 746, in test_named_target_resolves_real_commit_and_renders_its_own_registry\n    good = MODULE.Recovery.named_good(recovery, recovery.object)\n           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n  File \"/gate-target/repo/infra/forge/postgres-rollout.py\", line 400, in named_good\n    obj = self.rendered_postgres(source)\n          ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n  File \"/gate-target/repo/infra/forge/postgres-rollout.py\", line 409, in rendered_postgres\n    archive = subprocess.run(['git', '-C', self.args.repo, 'archive', source, 'infra/cluster', 'examples'], capture_output=True, timeout=60)\n              ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n  File \"/usr/lib/python3.11/subprocess.py\", line 550, in run\n    stdout, stderr = process.communicate(input, timeout=timeout)\n                     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n  File \"/usr/lib/python3.11/subprocess.py\", line 1207, in communicate\n    stdout, stderr = self._communicate(input, endtime, timeout)\n                     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n  File \"/usr/lib/python3.11/subprocess.py\", line 2060, in _communicate\n    self._check_timeout(endtime, orig_timeout, stdout, stderr)\n  File \"/usr/lib/python3.11/subprocess.py\", line 1251, in _check_timeout\n    raise TimeoutExpired(\nsubprocess.TimeoutExpired: Command '['git', '-C', '/gate-target/repo', 'archive', '4d7c4d9f731d0aed7381b2058f497fd16419b8bb', 'infra/cluster', 'examples']' timed out after 60 seconds\n\n----------------------------------------------------------------------\nRan 43 tests in 110.998s\n\nFAILED (errors=1)";

    /// Gate 35600cc4's failing check, entries and excerpt verbatim, with
    /// its own interval measured at `io_full_usec` of io.full in 110 s
    /// (the suite's "Ran 43 tests in 110.998s") — or, `None`, unmeasured,
    /// as the receipt recorded it.
    fn receipt_35600cc4(io_full_usec: Option<u64>, excerpt: &str) -> Value {
        let mut intervals = vec![];
        if let Some(io_full) = io_full_usec {
            intervals.push(json!({
                "from": format!("check-start:{GATE_35600CC4_CHECK}"),
                "to": format!("check-finish:{GATE_35600CC4_CHECK}"),
                "state": "measured",
                "elapsed_ns": 110_998_000_000_u64,
                "stall_usec": {"io.full": io_full},
            }));
        }
        json!({
            "verdict": "failed",
            "head": "4d7c4d9f731d0aed7381b2058f497fd16419b8bb",
            "checks": [{"name": GATE_35600CC4_CHECK, "result": "fail", "seconds": 111}],
            "fails": GATE_35600CC4_FAILS,
            "fails_excerpt": {GATE_35600CC4_CHECK: excerpt},
            "runtime_evidence": {"intervals": intervals},
        })
    }

    #[test]
    fn gate_35600cc4_a_unittest_error_whose_traceback_timed_out_is_equipment() {
        let f = failure_facts(
            "failed",
            Some(&receipt_35600cc4(Some(101_000_000), GATE_35600CC4_EXCERPT)),
        );
        assert_eq!(f.product_defect, None, "{f:?}");
        let eq = f
            .equipment_fault
            .expect("git archive's 60 s bound ran out under the stall");
        assert_eq!(eq.kind, EquipmentFaultKind::StallDeadline);
        assert_eq!(eq.checks, vec![GATE_35600CC4_CHECK.to_string()]);
        assert_eq!((eq.io_full_s, eq.wall_s), (Some(101), Some(110)));
        assert!(
            eq.why
                .contains("1 failure entry, every one equipment-shaped")
                && eq.why.contains("\"timeoutexpired\""),
            "the run.sh framing line is not an entry, and the why names the traceback's own words: {}",
            eq.why
        );
    }

    #[test]
    fn gate_35600cc4_under_the_line_or_unmeasured_stays_the_products() {
        let under = failure_facts(
            "failed",
            Some(&receipt_35600cc4(Some(59_000_000), GATE_35600CC4_EXCERPT)),
        );
        assert_eq!(under.equipment_fault, None, "{under:?}");
        assert!(under.product_defect.is_some(), "{under:?}");
        // As recorded: no interval for the check, so nothing waives it.
        let recorded = failure_facts(
            "failed",
            Some(&receipt_35600cc4(None, GATE_35600CC4_EXCERPT)),
        );
        assert_eq!(recorded.equipment_fault, None, "{recorded:?}");
        assert!(
            recorded
                .product_defect
                .as_ref()
                .is_some_and(|pd| pd.why.contains("no measured io.full stall")),
            "{recorded:?}"
        );
    }

    /// Only the traceback's EXCEPTION is read, never its source lines: the
    /// 35600cc4 traceback quotes `raise TimeoutExpired(` and `timeout=60`
    /// on its way down. An `AssertionError` is the branch's whatever its
    /// frames say, the way a Rust assertion is; a traceback the excerpt
    /// does not hold whole is no evidence, so it waives nothing.
    #[test]
    fn a_unittest_entry_is_the_drives_only_by_its_own_exception() {
        let asserted = GATE_35600CC4_EXCERPT.replace(
            "subprocess.TimeoutExpired: Command '['git', '-C', '/gate-target/repo', 'archive', '4d7c4d9f731d0aed7381b2058f497fd16419b8bb', 'infra/cluster', 'examples']' timed out after 60 seconds",
            "AssertionError: 'timed out' not found in 'ok'",
        );
        let f = failure_facts(
            "failed",
            Some(&receipt_35600cc4(Some(101_000_000), &asserted)),
        );
        assert_eq!(f.equipment_fault, None, "{f:?}");
        assert!(
            f.product_defect
                .as_ref()
                .is_some_and(|pd| pd.why.contains("AssertionError")),
            "{f:?}"
        );
        // A cut through the traceback: the exception is not on the record.
        let cut = GATE_35600CC4_EXCERPT.replace(
            "  File \"/usr/lib/python3.11/subprocess.py\", line 550",
            "... (+900 char(s) omitted - this check's excerpt budget was 6000 char(s); the Job log replay has the full text)\n  File \"/usr/lib/python3.11/subprocess.py\", line 550",
        );
        for excerpt in [cut.as_str(), ""] {
            let f = failure_facts(
                "failed",
                Some(&receipt_35600cc4(Some(101_000_000), excerpt)),
            );
            assert_eq!(f.equipment_fault, None, "{excerpt:?}: {f:?}");
            assert!(
                f.product_defect
                    .as_ref()
                    .is_some_and(|pd| pd.why.contains("traceback")),
                "{f:?}"
            );
        }
    }

    /// The framing line stands for every error line it counts; when it
    /// quotes fewer than it counts, the unquoted ones are unread and stay
    /// the product's.
    #[test]
    fn a_framing_line_that_quotes_fewer_than_it_counts_waives_nothing() {
        let mut r = receipt_35600cc4(Some(101_000_000), GATE_35600CC4_EXCERPT);
        r["fails"][0] = json!(
            "the-converge-watches-postgres: no cargo test failure in this check's output; 6 error line(s), first 5:"
        );
        let f = failure_facts("failed", Some(&r));
        assert!(f.equipment_fault.is_some(), "{f:?}");
        assert!(f.product_defect.is_some(), "{f:?}");
    }

    /// NOT DECLARED: a bare `max-time`, which the item also named. In this
    /// tree it is curl's FLAG — `--max-time "$MAX_TIME"` in a dozen
    /// scripts, and in test messages that quote the argv they checked —
    /// and the table's rule is that a phrase states a wait RAN OUT, never
    /// that one was configured (the bare `timeout` that matched a
    /// `timeout=1s` field). The verb that bounds itself with it says so in
    /// words that ARE declared: boss-step's "not answered within Ns (curl
    /// exit 28; BOSS_STEP_MAX_TIME)". The message below is the shape of
    /// a_killed_sender_leaves_no_header_behind's own check.
    #[test]
    fn a_message_that_quotes_the_max_time_flag_is_not_a_deadline() {
        let entry = "test: a_killed_sender::observe_units_bounds_its_post - panicked at crates/core/boss-testing/tests/a_killed_sender_leaves_no_header_behind.rs:1643:13: observe-units.sh \"20\": curl's --max-time: [\"--connect-timeout\", \"10\", \"--max-time\", \"30\"]";
        let f = failure_facts("failed", Some(&stalled_test(651_000_000, 1, &[entry])));
        assert_eq!(f.equipment_fault, None, "{f:?}");
        assert!(f.product_defect.is_some(), "{f:?}");
    }

    // -- a traceback quoted inside the exception (backlog 1cacf298) -------
    //
    // Review 26719580 of car 24cb04bc: the reader took the LAST
    // `Traceback (most recent call last):` anywhere in the block, so a
    // setUpClass AssertionError whose message quotes a child's traceback
    // was read by the CHILD's `subprocess.TimeoutExpired` and waived as
    // equipment. The receipt below is real output, not typed: five
    // unittest suites run under Python 3.11.2 in a dev-build worker
    // (2026-11-15, run f2b584b4), each in its own `::group::gate:` block,
    // then run.sh's failure-detail extractor lifted verbatim from
    // infra/gate-runner/run.sh, whose receipt is this file byte for byte.
    // It measured no interval; the 101 s io.full in 111 s each test adds
    // is synthetic, the same line the 35600cc4 cases above cross.
    //
    //   review-quoted        setUpClass AssertionError quoting a child's
    //                        TimeoutExpired traceback (the review's case)
    //   review-plain         the same assertion, nothing quoted
    //   chain-timeout-last   ValueError, then a real TimeoutExpired raised
    //                        while handling it
    //   chain-assertion-last a real TimeoutExpired, then an AssertionError
    //                        raised from it
    //   review-quoted-chain  an AssertionError quoting a child's traceback
    //                        that is itself the timeout-last chain

    const UNITTEST_QUOTED: &str = include_str!("failure_facts/unittest-quoted-traceback.json");

    /// The fixture receipt narrowed to ONE check — its entries and its
    /// excerpt, `excerpt` in place of it when given — with that check's
    /// interval measured at 101 s of io.full in 111 s.
    fn unittest_quoted(check: &str, excerpt: Option<&str>) -> Value {
        narrowed(UNITTEST_QUOTED, check, excerpt)
    }

    fn narrowed(fixture: &str, check: &str, excerpt: Option<&str>) -> Value {
        let all: Value = serde_json::from_str(fixture).expect("the extractor's JSON");
        let prefix = format!("{check}: ");
        let fails: Vec<Value> = all["fails"]
            .as_array()
            .expect("the extractor wrote fails")
            .iter()
            .filter(|e| e.as_str().is_some_and(|e| e.starts_with(&prefix)))
            .cloned()
            .collect();
        assert_eq!(fails.len(), 2, "{check}: framing line plus its ERROR line");
        let excerpt = excerpt.map_or_else(|| all["fails_excerpt"][check].clone(), |x| json!(x));
        json!({
            "verdict": "failed",
            "head": all["head"],
            "checks": [{"name": check, "result": "fail", "seconds": 111}],
            "fails": fails,
            "fails_excerpt": {check: excerpt},
            "runtime_evidence": {"intervals": [{
                "from": format!("check-start:{check}"),
                "to": format!("check-finish:{check}"),
                "state": "measured",
                "elapsed_ns": 111_000_000_000_u64,
                "stall_usec": {"io.full": 101_000_000},
            }]},
        })
    }

    fn excerpt_of(check: &str) -> String {
        let all: Value = serde_json::from_str(UNITTEST_QUOTED).expect("the extractor's JSON");
        all["fails_excerpt"][check]
            .as_str()
            .expect("the extractor wrote an excerpt")
            .to_string()
    }

    /// The product's, and said so: no equipment fault, and a defect whose
    /// why carries `says`.
    fn the_products(f: &FailureFacts, says: &str) {
        assert_eq!(f.equipment_fault, None, "waived: {f:?}");
        assert!(
            f.product_defect
                .as_ref()
                .is_some_and(|pd| pd.why.contains(says)),
            "expected a product defect saying {says:?}: {f:?}"
        );
    }

    #[test]
    fn an_assertion_that_quotes_a_childs_timeout_is_the_products() {
        let f = failure_facts("failed", Some(&unittest_quoted("review-quoted", None)));
        the_products(&f, "on an AssertionError");
        // Control: the same assertion with nothing quoted.
        let f = failure_facts("failed", Some(&unittest_quoted("review-plain", None)));
        the_products(&f, "on an AssertionError");
    }

    /// A quoted traceback that is itself a chain: its `During handling`
    /// line cannot be told from the real chain's, so the block is not
    /// read at all — ambiguous evidence waives nothing.
    #[test]
    fn an_assertion_that_quotes_a_chained_traceback_is_the_products() {
        let f = failure_facts(
            "failed",
            Some(&unittest_quoted("review-quoted-chain", None)),
        );
        // Its reason was the cut reading's too (backlog e27f7228).
        the_products_because(&f, "review-quoted-chain", UNWALKED_CHAIN);
    }

    /// Genuine chains, printed by unittest itself, still read their LAST
    /// link: the exception that failed the test.
    #[test]
    fn a_genuine_chain_is_read_by_its_last_exception() {
        let f = failure_facts("failed", Some(&unittest_quoted("chain-timeout-last", None)));
        assert_eq!(f.product_defect, None, "{f:?}");
        let eq = f
            .equipment_fault
            .expect("the chain's last link is the real TimeoutExpired");
        assert_eq!(eq.kind, EquipmentFaultKind::StallDeadline);
        assert_eq!(eq.checks, vec!["chain-timeout-last".to_string()]);
        let f = failure_facts(
            "failed",
            Some(&unittest_quoted("chain-assertion-last", None)),
        );
        the_products(&f, "on an AssertionError");
    }

    /// Cut evidence stays the product's: unittest's own `Traceback` line
    /// gone, so only the quoted one is left to read, and the excerpt's cut
    /// note inside the quoted traceback.
    #[test]
    fn a_cut_around_a_quoted_traceback_waives_nothing() {
        let whole = excerpt_of("review-quoted");
        let own = "----------------------------------------------------------------------\nTraceback (most recent call last):\n";
        assert!(whole.contains(own), "{whole}");
        let headless = whole.replacen(
            own,
            "----------------------------------------------------------------------\n",
            1,
        );
        let noted = whole.replacen(
            "  File \"/usr/lib/python3.11/subprocess.py\", line 1207",
            "... (+900 char(s) omitted - this check's excerpt budget was 6000 char(s); the Job log replay has the full text)\n  File \"/usr/lib/python3.11/subprocess.py\", line 1207",
            1,
        );
        for excerpt in [headless.as_str(), noted.as_str()] {
            let f = failure_facts(
                "failed",
                Some(&unittest_quoted("review-quoted", Some(excerpt))),
            );
            // A cut is still said as one (backlog e27f7228's control).
            the_products(
                &f,
                &format!(
                    "review-quoted failed on a unittest entry whose traceback the receipt's excerpt {NOT_WHOLE}"
                ),
            );
        }
    }

    // -- a quote that begins inside a chain (backlog 89ad2bf5) ------------
    //
    // Landed review 2199432a of car c11507c5: the reader took a Traceback
    // line as the test's next link whenever the three lines before it were
    // blank, a chain sentence, blank — so a quote that BEGINS mid-chain was
    // followed, and its child's TimeoutExpired waived the assertion. Real
    // output again: three unittest suites under Python 3.11.2 in this
    // run's dev-build worker (2026-11-16, run f9fc394d), through run.sh's
    // extractor lifted verbatim, as the fixture above was made.
    //
    //   tail-quoted          setUpClass AssertionError quoting the last 21
    //                        lines of a child's ValueError-then-timeout
    //                        chain (the review's measured case)
    //   cause-quoted         quoting whole a child that raised a timeout
    //                        `from` a ValueError never raised: its stderr
    //                        opens on the cause's line, not on a Traceback
    //   cause-quoted-inline  the same stderr as the assertion's WHOLE
    //                        message, so the quote's first line shares the
    //                        AssertionError's line and the join after it
    //                        has a genuine chain's shape

    const UNITTEST_QUOTED_LINK: &str =
        include_str!("failure_facts/unittest-quoted-chain-link.json");

    fn quoted_link_is_the_products(check: &str, reason: &str) {
        let f = failure_facts("failed", Some(&narrowed(UNITTEST_QUOTED_LINK, check, None)));
        the_products_because(&f, check, reason);
    }

    // -- each refusal says its own reason (backlog e27f7228) --------------
    //
    // Landed review of car 8109b041, finding 1: the two refusals that car
    // added — a chain sentence the walk did not take, and a last link whose
    // first frame is not the test's — came back as the CUT reading, whose
    // reason says the excerpt does not hold the traceback whole, beside a
    // traceback held whole. The tests asserted only the prefix, so nothing
    // read the reason. These pin it.

    /// What the unwalked-chain refusal says, and the link refusal.
    const UNWALKED_CHAIN: &str =
        "on a unittest traceback whose chain sentence does not follow one exception line";
    const FOREIGN_LINK: &str =
        "on a unittest traceback whose last chained link does not begin in the test's own function";
    /// What the cut refusal says: never said of a traceback held whole.
    const NOT_WHOLE: &str = "does not hold whole";

    /// The product's, and for `reason`, in the check's own defect line —
    /// never for the cut reading's.
    fn the_products_because(f: &FailureFacts, check: &str, reason: &str) {
        the_products(f, &format!("{check} failed {reason}"));
        let why = f.product_defect.as_ref().map(|pd| pd.why.as_str());
        assert!(
            why.is_some_and(|why| !why.contains(NOT_WHOLE)),
            "a traceback held whole is read as cut: {why:?}"
        );
    }

    #[test]
    fn an_assertion_quoting_the_tail_of_a_childs_chain_is_the_products() {
        quoted_link_is_the_products("tail-quoted", UNWALKED_CHAIN);
    }

    #[test]
    fn an_assertion_quoting_a_never_raised_cause_is_the_products() {
        quoted_link_is_the_products("cause-quoted", UNWALKED_CHAIN);
    }

    #[test]
    fn a_cause_quoted_on_the_assertions_own_line_is_the_products() {
        quoted_link_is_the_products("cause-quoted-inline", FOREIGN_LINK);
    }

    /// The walk's other edge: a GENUINE link of several lines (an
    /// exception note, a message with a newline) ends the walk early, and
    /// the link it stops on is a TimeoutExpired while the test failed on
    /// the AssertionError raised from it. The chain sentence the walk did
    /// not take refuses the block rather than read the wrong link.
    #[test]
    fn a_genuine_link_of_several_lines_waives_nothing() {
        let whole = excerpt_of("chain-assertion-last");
        let join = " seconds\n\nThe above exception was the direct cause";
        assert!(whole.contains(join), "{whole}");
        let noted = whole.replacen(
            join,
            " seconds\nraised while the reading was retried\n\nThe above exception was the direct cause",
            1,
        );
        let f = failure_facts(
            "failed",
            Some(&unittest_quoted("chain-assertion-last", Some(&noted))),
        );
        the_products_because(&f, "chain-assertion-last", UNWALKED_CHAIN);
    }

    /// AN UNJUDGED RED IS NEVER A PRODUCT DEFECT (decided by the
    /// conductor on c02fc3b3, 2026-11-04). A red with no readable verdict
    /// — a receipt that is not JSON, run.sh's `verdict: unreadable`, or
    /// an `error` with no `fails` (the shape `train_gate::no_verdict`
    /// names, gate-run df02648a) — judged nothing about the tree, so an
    /// immutable product_defect would strike a branch on no evidence. It
    /// records `equipment_fault` kind `unjudged`, naming what was
    /// unreadable, and nothing measured.
    #[test]
    fn a_red_with_no_readable_verdict_is_an_unjudged_equipment_fault() {
        let cases = [
            (None, "not readable JSON"),
            (
                Some(json!({"verdict": "unreadable",
                            "error": "[Errno 2] No such file or directory: '/gate-target/receipt.json'"})),
                "the gate's receipt is unreadable: [Errno 2] No such file",
            ),
            (
                Some(json!({"verdict": "unreadable"})),
                "the gate's receipt is unreadable: it names no error",
            ),
            (
                Some(
                    json!({"verdict": "failed", "fails": [], "error": "runner died before gate.sh"}),
                ),
                "the gate's receipt names no failing check: runner died before gate.sh",
            ),
        ];
        for (receipt, said) in cases {
            let f = failure_facts("failed", receipt.as_ref());
            assert_eq!(
                f.product_defect, None,
                "{receipt:?}: no product defect, ever"
            );
            let eq = f
                .equipment_fault
                .unwrap_or_else(|| panic!("{receipt:?}: an unjudged red is recorded"));
            assert_eq!(eq.kind, EquipmentFaultKind::Unjudged, "{receipt:?}");
            assert!(eq.why.contains(said), "{receipt:?}: {}", eq.why);
            assert_eq!((eq.io_full_s, eq.wall_s), (None, None), "nothing measured");
            assert!(eq.checks.is_empty(), "{receipt:?}");
        }
        let written = failure_facts("failed", None).fields();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].1["kind"], "unjudged");
    }

    /// A RED THAT NAMES NOTHING IS UNJUDGED TOO (the conductor on
    /// c02fc3b3, 2026-11-04). A `failed` receipt with no `fails`, no
    /// failing check and no `error` evidences no defect, and
    /// product_defect governs the strike, so writing one would be the
    /// immutable fact on no evidence that was ruled out.
    #[test]
    fn a_red_that_names_no_failing_check_and_no_error_is_unjudged() {
        let cases = [
            json!({"verdict": "failed"}),
            json!({"verdict": "failed", "fails": [],
                   "checks": [{"name": "clippy", "result": "pass"}]}),
            json!({"verdict": "failed", "fails": [],
                   "checks": [{"name": "a-lint", "result": "refused"}]}),
        ];
        for r in cases {
            let f = failure_facts("failed", Some(&r));
            assert_eq!(f.product_defect, None, "{r}: no product defect");
            let eq = f.equipment_fault.unwrap_or_else(|| panic!("{r}: recorded"));
            assert_eq!(eq.kind, EquipmentFaultKind::Unjudged, "{r}");
            assert!(eq.why.contains("names no failing check"), "{r}: {}", eq.why);
            assert_eq!((eq.io_full_s, eq.wall_s), (None, None));
        }
        // The control: one failing check with no entry and no interval is
        // named, so it is judged — the product's, as before.
        let named = json!({"verdict": "failed", "fails": [],
                           "checks": [{"name": "svelte-check", "result": "fail"}]});
        let f = failure_facts("failed", Some(&named));
        assert!(f.product_defect.is_some(), "{f:?}");
        assert_eq!(f.equipment_fault, None, "{f:?}");
    }

    /// The control: a red that NAMES its failures is judged, and keeps
    /// the per-entry rule even when it also carries an `error`.
    #[test]
    fn a_red_with_fails_and_an_error_is_judged_per_entry() {
        let mut r = receipt_0de50487(59_000_000);
        r["error"] = json!("a note beside the fails");
        let f = failure_facts("failed", Some(&r));
        assert!(f.product_defect.is_some(), "{f:?}");
        assert_eq!(f.equipment_fault, None, "{f:?}");
    }

    #[test]
    fn the_facts_are_written_in_the_declared_shape() {
        let mut r = receipt_0de50487(651_000_000);
        r["fails"].as_array_mut().unwrap().push(json!(CONSERVATION));
        let fields = failure_facts("failed", Some(&r)).fields();
        let keys: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec![EQUIPMENT_FAULT, PRODUCT_DEFECT]);
        let eq = &fields[0].1;
        assert_eq!(eq["kind"], "stall-deadline");
        for key in ["kind", "checks", "io_full_s", "wall_s", "why"] {
            assert!(eq.get(key).is_some(), "equipment_fault carries {key}: {eq}");
        }
        let pd = &fields[1].1;
        for key in ["checks", "entries", "why"] {
            assert!(pd.get(key).is_some(), "product_defect carries {key}: {pd}");
        }
        let lost = failure_facts("lost", None).fields();
        assert_eq!(lost[0].1["kind"], "lost");
        assert_eq!(lost[0].1["io_full_s"], Value::Null);
        assert!(failure_facts("green", None).fields().is_empty());
    }

    /// A WRITE REPLACES BOTH FACTS (review 2e95708a R1). The step merge
    /// door keeps a key it is not sent, so a retried verdict that names
    /// only its present fact would leave a stale one from an interrupted
    /// first attempt beside it. The write names BOTH keys every time: a
    /// present fact as its object, an absent one as null, which the door
    /// deletes in the same PATCH.
    #[test]
    fn the_write_names_both_facts_and_nulls_the_absent_one() {
        let keys = |w: &[(&'static str, Value)]| w.iter().map(|(k, _)| *k).collect::<Vec<_>>();
        let lost = failure_facts("lost", None).writes();
        assert_eq!(keys(&lost), vec![EQUIPMENT_FAULT, PRODUCT_DEFECT]);
        assert_eq!(lost[0].1["kind"], "lost");
        assert_eq!(
            lost[1].1,
            Value::Null,
            "the absent product defect is deleted"
        );
        let mut r = receipt_0de50487(59_000_000);
        r["fails"].as_array_mut().unwrap().push(json!(CONSERVATION));
        let product = failure_facts("failed", Some(&r)).writes();
        assert_eq!(
            product[0].1,
            Value::Null,
            "the absent equipment fault is deleted"
        );
        assert!(product[1].1.is_object());
        for word in ["green", "withdrawn"] {
            let none = failure_facts(word, None).writes();
            assert_eq!(keys(&none), vec![EQUIPMENT_FAULT, PRODUCT_DEFECT], "{word}");
            assert!(none.iter().all(|(_, v)| v.is_null()), "{word}: {none:?}");
        }
    }
}
