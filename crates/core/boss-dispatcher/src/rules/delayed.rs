//! The delayed-admission DECLARATION and its identities — car 1 of the
//! finish line on backlog 84a010cb (design f1d66249, folded into
//! docs/architecture-decisions.md "Durable source-relative guarded
//! admission").
//!
//! Pure, no I/O. Three things, each a value a later car consumes rather
//! than re-derives:
//!
//! - [`ElapsedDelay`]: a rule's `delay` read as an EXACT count of
//!   seconds. Seven days is 604800 seconds from the recorded source fact,
//!   independent of daylight saving and of any later triage or
//!   heartbeat, so the unit table is fixed arithmetic and there is no
//!   calendar in it. Zero, a sign, a fraction, whitespace, an unknown
//!   unit and an overflowing count are refused, never rounded.
//! - [`DelayedDeclaration`]: the one rule shape the contract supports
//!   first — an event-driven rule with exactly ONE `jobs.spawn` and a
//!   positive fixed delay. A periodic rule with a delay, a delayed rule
//!   with several effects or none, and any other handler are refused
//!   with the reason named.
//! - [`IntentKey`]: the domain-separated UUIDv5 of one logical
//!   admission (partition, source event, rule name and version, handler
//!   ordinal), and the separate UUIDv5 of its child derived from that
//!   intent. The same source and rule always name the same intent and
//!   the same child — including on a retry after that child closed — so
//!   a retry reconciles an existing child rather than admitting another.
//!
//! What this module does NOT do: execute anything. Nothing captures an
//! intent, holds it or drains it yet, so `authoring::validate` still
//! refuses every rule declaring a delay. It now refuses a malformed or
//! unsupported one with the precise reason ([`DelayRefusal`]) and a
//! well-formed one as not executed yet — fail closed either way, until
//! the last car of the finish line relaxes it.

use chrono::{DateTime, TimeDelta, Utc};
use thiserror::Error;
use uuid::Uuid;

use super::registry::RawRule;

/// The one handler a delayed rule may declare (design f1d66249: "the
/// first supported declaration is one event-driven `jobs.spawn`").
pub const DELAYED_HANDLER: &str = "jobs.spawn";

/// A rule's `delay`, read as an exact positive number of seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ElapsedDelay {
    seconds: i64,
}

impl ElapsedDelay {
    /// Read `<digits><unit>`, unit one of `s`, `m`, `h`, `d`. Nothing
    /// else: no sign, no fraction, no whitespace, no compound spelling,
    /// no unitless count — each of those is a claim whose meaning a
    /// reader would have to guess.
    pub fn parse(src: &str) -> Result<Self, DelayRefusal> {
        let malformed = || DelayRefusal::Malformed {
            src: src.to_string(),
        };
        let overflow = || DelayRefusal::Overflow {
            src: src.to_string(),
        };
        if src.is_empty() {
            return Err(DelayRefusal::Empty);
        }
        if src.starts_with(['-', '+']) {
            return Err(DelayRefusal::Signed {
                src: src.to_string(),
            });
        }
        // The unit is the last char; a non-ASCII last char cannot be one.
        let (count, unit) = src.split_at(src.len() - src.chars().last().map_or(0, char::len_utf8));
        let per_unit: i64 = match unit {
            "s" => 1,
            "m" => 60,
            "h" => 3_600,
            "d" => 86_400,
            _ => return Err(malformed()),
        };
        // ASCII digits only, checked explicitly: `str::parse::<u64>`
        // would accept a leading `+`, which is a different spelling.
        if count.is_empty() || !count.bytes().all(|b| b.is_ascii_digit()) {
            return Err(malformed());
        }
        let n: i64 = count.parse().map_err(|_| overflow())?;
        let seconds = n.checked_mul(per_unit).ok_or_else(overflow)?;
        if seconds == 0 {
            return Err(DelayRefusal::Zero {
                src: src.to_string(),
            });
        }
        // Representable as a duration, or it is not a delay at all.
        TimeDelta::try_seconds(seconds).ok_or_else(overflow)?;
        Ok(Self { seconds })
    }

    /// The exact elapsed seconds this delay declares.
    pub fn seconds(&self) -> i64 {
        self.seconds
    }

    /// The fixed due instant: the source fact's own timestamp plus this
    /// delay, computed once from that anchor. A sum past the end of the
    /// representable calendar is refused, never saturated.
    pub fn due_at(&self, anchor: DateTime<Utc>) -> Result<DateTime<Utc>, DelayRefusal> {
        let refused = || DelayRefusal::DueOverflow {
            anchor,
            seconds: self.seconds,
        };
        let span = TimeDelta::try_seconds(self.seconds).ok_or_else(refused)?;
        anchor.checked_add_signed(span).ok_or_else(refused)
    }
}

/// Whether work due at `due_at` may execute at `now`. At or after the
/// bound, never before it: the exact instant is due.
pub fn is_due(due_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now >= due_at
}

/// The supported delayed declaration of one rule, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelayedDeclaration {
    pub rule: String,
    pub version: u32,
    /// The event topic pattern the rule listens on.
    pub on_event: String,
    /// The ordinal of the delayed effect within the rule's `do` list.
    /// Always 0 today (exactly one effect is supported), and part of
    /// the intent identity so a later multi-effect contract cannot
    /// collide with it.
    pub handler_ordinal: u32,
    pub delay: ElapsedDelay,
}

impl DelayedDeclaration {
    /// `Ok(None)` for a rule with no delay — an ordinary immediate or
    /// periodic rule, untouched. `Ok(Some(_))` for the supported shape.
    /// Every other rule declaring a delay is refused with its reason.
    pub fn from_raw(raw: &RawRule) -> Result<Option<Self>, DelayRefusal> {
        let Some(src) = &raw.delay else {
            return Ok(None);
        };
        // The shape first, then the timing: a periodic rule is refused
        // as periodic whatever its delay says, so the author fixes the
        // thing that would still be wrong after fixing the spelling.
        if raw.schedule.is_some() {
            return Err(DelayRefusal::Periodic);
        }
        let [effect] = raw.do_steps.as_slice() else {
            return Err(DelayRefusal::EffectCount {
                count: raw.do_steps.len(),
            });
        };
        if effect.handler != DELAYED_HANDLER {
            return Err(DelayRefusal::UnsupportedHandler {
                handler: effect.handler.clone(),
            });
        }
        // Never `Ok(None)` for a rule that declares a delay: a later car
        // reading `None` as "an immediate rule" would fire it at once.
        let Some(on_event) = &raw.on_event else {
            return Err(DelayRefusal::NoEventTrigger);
        };
        let delay = ElapsedDelay::parse(src)?;
        Ok(Some(Self {
            rule: raw.name.clone(),
            version: raw.version,
            on_event: on_event.clone(),
            handler_ordinal: 0,
            delay,
        }))
    }
}

/// Why a declared delay is refused. Each names what the author wrote,
/// so a refusal is a verdict the reader does not have to re-derive.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DelayRefusal {
    #[error("the delay is empty")]
    Empty,
    #[error(
        "{src:?} is not <digits><unit> with unit one of s, m, h, d (an exact elapsed \
         duration, e.g. \"7d\" = 604800 seconds)"
    )]
    Malformed { src: String },
    #[error("{src:?} is signed: a delay is an unsigned positive elapsed duration")]
    Signed { src: String },
    #[error("{src:?} is zero: a delay is positive")]
    Zero { src: String },
    #[error("{src:?} overflows the representable range of seconds")]
    Overflow { src: String },
    #[error("the due instant {anchor} + {seconds}s is past the representable calendar")]
    DueOverflow { anchor: DateTime<Utc>, seconds: i64 },
    #[error(
        "a periodic (schedule) rule cannot declare a delay: a delay is measured from one \
         source event"
    )]
    Periodic,
    #[error("a delayed rule is triggered by an event (on_event), and this one names none")]
    NoEventTrigger,
    #[error("a delayed rule declares exactly one effect; this one declares {count}")]
    EffectCount { count: usize },
    #[error("a delayed rule's one effect is {DELAYED_HANDLER}; this one is {handler:?}")]
    UnsupportedHandler { handler: String },
}

/// One logical delayed admission: the source fact and the rule effect it
/// was captured for. Every field is required provenance — an empty one
/// is refused at construction, so no identity is ever derived from a
/// missing binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentKey {
    partition: String,
    source_event: String,
    rule: String,
    rule_version: u32,
    handler_ordinal: u32,
}

/// The namespace of intent ids. Fixed forever: changing it re-keys every
/// captured intent. Distinct from [`CHILD_NAMESPACE`] and from every
/// other derived id in the dispatcher (jobs_spawn's delegated child).
pub const INTENT_NAMESPACE: Uuid = Uuid::from_u128(0x84a0_10cb_f1d6_4249_8000_0000_0000_0001);
/// The namespace of child ids, derived from an intent id.
pub const CHILD_NAMESPACE: Uuid = Uuid::from_u128(0x84a0_10cb_f1d6_4249_8000_0000_0000_0002);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("an intent identity needs its {field}, and it is empty")]
pub struct MissingProvenance {
    pub field: &'static str,
}

impl IntentKey {
    pub fn new(
        partition: &str,
        source_event: &str,
        rule: &str,
        rule_version: u32,
        handler_ordinal: u32,
    ) -> Result<Self, MissingProvenance> {
        for (field, value) in [
            ("partition", partition),
            ("source_event", source_event),
            ("rule", rule),
        ] {
            if value.is_empty() {
                return Err(MissingProvenance { field });
            }
        }
        Ok(Self {
            partition: partition.to_string(),
            source_event: source_event.to_string(),
            rule: rule.to_string(),
            rule_version,
            handler_ordinal,
        })
    }

    /// The intent's id: UUIDv5 under [`INTENT_NAMESPACE`] of the fields
    /// as one canonical JSON array. An array, not a joined string: a
    /// rule name may contain `:`, and JSON's quoting makes every split
    /// of the bytes unambiguous.
    pub fn intent_id(&self) -> Uuid {
        let name = serde_json::json!([
            self.partition,
            self.source_event,
            self.rule,
            self.rule_version,
            self.handler_ordinal,
        ])
        .to_string();
        Uuid::new_v5(&INTENT_NAMESPACE, name.as_bytes())
    }

    /// The child's id: a separate UUIDv5 of the intent id, under
    /// [`CHILD_NAMESPACE`]. Depends on nothing but the intent, so every
    /// attempt — before or after the child closed — names this child.
    pub fn child_id(&self) -> Uuid {
        child_id_of(self.intent_id())
    }
}

/// The child id of an intent, from the intent id alone.
pub fn child_id_of(intent: Uuid) -> Uuid {
    Uuid::new_v5(&CHILD_NAMESPACE, intent.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::registry::{Cadence, RawDoStep, RawSchedule};
    use chrono::TimeZone;

    fn secs(src: &str) -> i64 {
        ElapsedDelay::parse(src)
            .unwrap_or_else(|e| panic!("{src:?} should parse: {e}"))
            .seconds()
    }

    /// The packet's own boundary: seven days is exactly 604800 seconds,
    /// and the other spellings of it agree.
    #[test]
    fn seven_days_is_exactly_604800_seconds_in_every_unit() {
        assert_eq!(secs("7d"), 604_800);
        assert_eq!(secs("168h"), 604_800);
        assert_eq!(secs("10080m"), 604_800);
        assert_eq!(secs("604800s"), 604_800);
        assert_eq!(secs("1s"), 1);
        assert_eq!(secs("5m"), 300);
        assert_eq!(secs("2h"), 7_200);
    }

    #[test]
    fn a_zero_signed_or_malformed_delay_is_refused_with_its_reason() {
        let cases: &[(&str, fn(&DelayRefusal) -> bool)] = &[
            ("", |r| matches!(r, DelayRefusal::Empty)),
            ("0", |r| matches!(r, DelayRefusal::Malformed { .. })),
            ("0d", |r| matches!(r, DelayRefusal::Zero { .. })),
            ("000s", |r| matches!(r, DelayRefusal::Zero { .. })),
            ("-7d", |r| matches!(r, DelayRefusal::Signed { .. })),
            ("+7d", |r| matches!(r, DelayRefusal::Signed { .. })),
            ("7", |r| matches!(r, DelayRefusal::Malformed { .. })),
            ("d", |r| matches!(r, DelayRefusal::Malformed { .. })),
            ("7w", |r| matches!(r, DelayRefusal::Malformed { .. })),
            ("7D", |r| matches!(r, DelayRefusal::Malformed { .. })),
            ("1.5d", |r| matches!(r, DelayRefusal::Malformed { .. })),
            (" 7d", |r| matches!(r, DelayRefusal::Malformed { .. })),
            ("7d ", |r| matches!(r, DelayRefusal::Malformed { .. })),
            ("7 d", |r| matches!(r, DelayRefusal::Malformed { .. })),
            ("1d12h", |r| matches!(r, DelayRefusal::Malformed { .. })),
            ("seven days", |r| {
                matches!(r, DelayRefusal::Malformed { .. })
            }),
            ("P7D", |r| matches!(r, DelayRefusal::Malformed { .. })),
            ("٧d", |r| matches!(r, DelayRefusal::Malformed { .. })),
        ];
        for (src, is) in cases {
            let err = ElapsedDelay::parse(src).expect_err(&format!("{src:?} must be refused"));
            assert!(is(&err), "{src:?} refused for the wrong reason: {err:?}");
        }
    }

    /// Overflow is refused at every place it can happen: the digit
    /// count itself, the unit multiplication, and the range a duration
    /// can represent — never wrapped or saturated into a different
    /// claim.
    #[test]
    fn an_overflowing_delay_is_refused_not_wrapped() {
        for src in [
            "99999999999999999999999s",
            &format!("{}d", u64::MAX),
            &format!("{}s", i64::MAX),
            &format!("{}d", i64::MAX / 86_400 + 1),
        ] {
            let err = ElapsedDelay::parse(src).expect_err(&format!("{src} must be refused"));
            assert!(
                matches!(err, DelayRefusal::Overflow { .. }),
                "{src}: {err:?}"
            );
        }
    }

    /// Before / exact / after the seven-day bound, from the source
    /// fact's own timestamp — the exact instant is due, one second
    /// before it is not.
    #[test]
    fn the_due_bound_is_the_anchor_plus_the_delay_and_the_exact_instant_is_due() {
        let anchor = Utc.with_ymd_and_hms(2026, 10, 12, 15, 59, 10).unwrap();
        let due = ElapsedDelay::parse("7d").unwrap().due_at(anchor).unwrap();
        assert_eq!(due, Utc.with_ymd_and_hms(2026, 10, 19, 15, 59, 10).unwrap());
        assert!(!is_due(due, due - TimeDelta::seconds(1)), "before");
        assert!(is_due(due, due), "exact");
        assert!(is_due(due, due + TimeDelta::seconds(1)), "after");
        // Fixed elapsed time, not wall-clock days: a span crossing the
        // US daylight-saving change (2026-11-01) is still 604800 s.
        let across = Utc.with_ymd_and_hms(2026, 10, 29, 12, 0, 0).unwrap();
        let due = ElapsedDelay::parse("7d").unwrap().due_at(across).unwrap();
        assert_eq!((due - across).num_seconds(), 604_800);
    }

    #[test]
    fn a_due_instant_past_the_calendar_is_refused() {
        let end = DateTime::<Utc>::MAX_UTC - TimeDelta::seconds(10);
        let err = ElapsedDelay::parse("1d")
            .unwrap()
            .due_at(end)
            .expect_err("past the calendar");
        assert!(matches!(err, DelayRefusal::DueOverflow { .. }), "{err:?}");
    }

    fn rule(delay: Option<&str>, handlers: &[&str]) -> RawRule {
        RawRule {
            name: "follow-up-an-unanswered-inquiry".into(),
            on_event: Some("jobs.job.opened".into()),
            schedule: None,
            when: None,
            do_steps: handlers
                .iter()
                .map(|h| RawDoStep {
                    handler: (*h).into(),
                    args: Default::default(),
                })
                .collect(),
            delay: delay.map(str::to_string),
            version: 4,
            why: None,
        }
    }

    #[test]
    fn the_supported_declaration_is_one_event_driven_jobs_spawn() {
        let d = DelayedDeclaration::from_raw(&rule(Some("7d"), &["jobs.spawn"]))
            .expect("supported")
            .expect("a delayed declaration");
        assert_eq!(d.rule, "follow-up-an-unanswered-inquiry");
        assert_eq!(d.version, 4);
        assert_eq!(d.on_event, "jobs.job.opened");
        assert_eq!(d.handler_ordinal, 0);
        assert_eq!(d.delay.seconds(), 604_800);
    }

    /// Positive controls: an immediate rule and a periodic rule with no
    /// delay are not delayed declarations, whatever their handlers.
    #[test]
    fn a_rule_without_a_delay_is_not_a_delayed_declaration() {
        assert_eq!(
            DelayedDeclaration::from_raw(&rule(None, &["jobs.spawn", "assign_step"])),
            Ok(None)
        );
        let mut periodic = rule(None, &["jobs.spawn"]);
        periodic.on_event = None;
        periodic.schedule = Some(RawSchedule {
            cadence: Cadence::parse("daily").unwrap(),
            anchor_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            business_calendar: None,
        });
        assert_eq!(DelayedDeclaration::from_raw(&periodic), Ok(None));
    }

    #[test]
    fn an_unsupported_delayed_shape_is_refused_with_its_reason() {
        let mut periodic = rule(Some("7d"), &["jobs.spawn"]);
        periodic.on_event = None;
        periodic.schedule = Some(RawSchedule {
            cadence: Cadence::parse("daily").unwrap(),
            anchor_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            business_calendar: None,
        });
        assert_eq!(
            DelayedDeclaration::from_raw(&periodic),
            Err(DelayRefusal::Periodic)
        );
        assert_eq!(
            DelayedDeclaration::from_raw(&rule(Some("7d"), &["jobs.spawn", "jobs.spawn"])),
            Err(DelayRefusal::EffectCount { count: 2 })
        );
        assert_eq!(
            DelayedDeclaration::from_raw(&rule(Some("7d"), &[])),
            Err(DelayRefusal::EffectCount { count: 0 })
        );
        assert_eq!(
            DelayedDeclaration::from_raw(&rule(Some("7d"), &["assign_step"])),
            Err(DelayRefusal::UnsupportedHandler {
                handler: "assign_step".into()
            })
        );
        assert!(matches!(
            DelayedDeclaration::from_raw(&rule(Some("seven days"), &["jobs.spawn"])),
            Err(DelayRefusal::Malformed { .. })
        ));
        // A delay with no trigger at all is refused here too — never
        // answered `Ok(None)`, which a later reader would take for an
        // immediate rule.
        let mut orphan = rule(Some("7d"), &["jobs.spawn"]);
        orphan.on_event = None;
        assert_eq!(
            DelayedDeclaration::from_raw(&orphan),
            Err(DelayRefusal::NoEventTrigger)
        );
    }

    fn key(partition: &str, event: &str, rule: &str, version: u32, ordinal: u32) -> IntentKey {
        IntentKey::new(partition, event, rule, version, ordinal).unwrap()
    }

    const EVENT: &str = "0193f0aa-1111-7000-8000-000000000001";

    /// The ids are pinned as VALUES, not only as equalities: a changed
    /// namespace or encoding re-keys every captured intent, and an
    /// equality test between two calls of the new code would not see
    /// it. If this fails, the change is a migration, not a refactor.
    #[test]
    fn an_intent_and_its_child_have_fixed_ids_from_their_inputs() {
        let k = key("algedonic", EVENT, "follow-up-an-unanswered-inquiry", 4, 0);
        let intent = k.intent_id();
        assert_eq!(intent.get_version_num(), 5);
        assert_eq!(k.child_id().get_version_num(), 5);
        assert_eq!(
            intent,
            Uuid::new_v5(
                &INTENT_NAMESPACE,
                br#"["algedonic","0193f0aa-1111-7000-8000-000000000001","follow-up-an-unanswered-inquiry",4,0]"#
            )
        );
        assert_eq!(
            k.child_id(),
            Uuid::new_v5(&CHILD_NAMESPACE, intent.as_bytes())
        );
        assert_ne!(intent, k.child_id(), "the child is not the intent");
        // The literals, computed independently (Python's uuid.uuid5 over
        // the same namespace and bytes, 2026-10-20), so a change to the
        // encoding above cannot pass by changing both sides at once.
        assert_eq!(intent.to_string(), "6c0457c8-d89b-5644-bc69-ef17bae660e4");
        assert_eq!(
            k.child_id().to_string(),
            "201c39d7-5548-5ac5-a1f2-3f4402338272"
        );
    }

    /// One logical source/rule admission yields one child: the same
    /// inputs on any retry — including one after the child closed, which
    /// this function cannot even observe — name the same intent and
    /// child.
    #[test]
    fn a_retry_names_the_same_intent_and_child() {
        let a = key("algedonic", EVENT, "r", 4, 0);
        let b = key("algedonic", EVENT, "r", 4, 0);
        assert_eq!(a.intent_id(), b.intent_id());
        assert_eq!(a.child_id(), b.child_id());
        assert_eq!(child_id_of(a.intent_id()), b.child_id());
    }

    /// Every bound field separates identities, and the encoding is not a
    /// joined string: a rule name may itself contain `:` (rule names are
    /// `:`-separated segments), so `a:b` + `c` must not equal `a` +
    /// `b:c`.
    #[test]
    fn every_bound_field_separates_identities() {
        let base = key("algedonic", EVENT, "r", 4, 0);
        let variants = [
            key("playground", EVENT, "r", 4, 0),
            key(
                "algedonic",
                "0193f0aa-1111-7000-8000-000000000002",
                "r",
                4,
                0,
            ),
            key("algedonic", EVENT, "s", 4, 0),
            key("algedonic", EVENT, "r", 5, 0),
            key("algedonic", EVENT, "r", 4, 1),
        ];
        let mut seen = std::collections::HashSet::new();
        assert!(seen.insert(base.intent_id()));
        assert!(seen.insert(base.child_id()));
        for v in &variants {
            assert!(seen.insert(v.intent_id()), "{v:?} collides");
            assert!(seen.insert(v.child_id()), "{v:?} child collides");
        }
        assert_ne!(
            key("a:b", "c", "r", 1, 0).intent_id(),
            key("a", "b:c", "r", 1, 0).intent_id()
        );
        assert_ne!(
            key("p", "e", "a:b", 1, 0).intent_id(),
            key("p", "e:a", "b", 1, 0).intent_id()
        );
    }

    #[test]
    fn the_two_namespaces_are_distinct_from_each_other_and_from_jobs_spawn() {
        assert_ne!(INTENT_NAMESPACE, CHILD_NAMESPACE);
        // jobs_spawn's delegated-child namespace, read as its literal.
        let delegated = Uuid::from_u128(0x5a5a_d7c1_0000_4000_8000_5583_96ff_0001);
        assert_ne!(INTENT_NAMESPACE, delegated);
        assert_ne!(CHILD_NAMESPACE, delegated);
    }

    /// No identity is derived from a missing binding.
    #[test]
    fn an_empty_binding_is_refused_not_hashed() {
        assert_eq!(
            IntentKey::new("", EVENT, "r", 1, 0),
            Err(MissingProvenance { field: "partition" })
        );
        assert_eq!(
            IntentKey::new("p", "", "r", 1, 0),
            Err(MissingProvenance {
                field: "source_event"
            })
        );
        assert_eq!(
            IntentKey::new("p", EVENT, "", 1, 0),
            Err(MissingProvenance { field: "rule" })
        );
    }
}
