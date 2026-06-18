//! ICS (RFC 5545) calendar-feed generator for a tech's schedule.
//!
//! Pure function: takes the tech's assignments and availability, returns
//! a string containing a full VCALENDAR. Callers are responsible for
//! window-scoping the input rows (we include everything passed in).
//!
//! The output is intentionally minimal — no RRULE, no VTIMEZONE. We emit
//! UTC timestamps (`...Z`) for every event, which every calendar client
//! handles without a timezone definition. If a tech's client is set to
//! a local timezone, the events display in local time automatically.

use super::types::{
    AssignmentKind, AssignmentStatus, AvailabilityKind, ScheduledAssignment, TechAvailability,
};
use chrono::{DateTime, Utc};

/// Build an ICS document for one tech's schedule.
///
/// `now` is folded into the DTSTAMP field on each event — passing it in
/// keeps the function deterministic under test. `employee_id` is embedded
/// in the calendar PRODID so clients show a distinct source.
pub fn build_ics(
    employee_id: &str,
    assignments: &[ScheduledAssignment],
    availability: &[TechAvailability],
    now: DateTime<Utc>,
) -> String {
    let mut out = String::new();
    out.push_str("BEGIN:VCALENDAR\r\n");
    out.push_str("VERSION:2.0\r\n");
    out.push_str(&format!(
        "PRODID:-//Boss//tech/{}//EN\r\n",
        escape_text(employee_id)
    ));
    out.push_str("CALSCALE:GREGORIAN\r\n");
    out.push_str("METHOD:PUBLISH\r\n");
    out.push_str(&format!(
        "X-WR-CALNAME:Boss — {}\r\n",
        escape_text(employee_id)
    ));

    for a in assignments {
        write_assignment(&mut out, a, now);
    }
    for av in availability {
        if should_include_availability(av.kind) {
            write_availability(&mut out, av, now);
        }
    }

    out.push_str("END:VCALENDAR\r\n");
    out
}

/// PTO / training / sick / holiday block the tech's time. "Available"
/// slots are their work schedule — we don't surface those as calendar
/// events because the tech doesn't want "I'm at work 8–5" on their
/// personal calendar. "Blocked" is the operational analogue of PTO.
fn should_include_availability(kind: AvailabilityKind) -> bool {
    matches!(
        kind,
        AvailabilityKind::Pto
            | AvailabilityKind::Sick
            | AvailabilityKind::Holiday
            | AvailabilityKind::Training
            | AvailabilityKind::Blocked
    )
}

fn write_assignment(out: &mut String, a: &ScheduledAssignment, now: DateTime<Utc>) {
    let summary = format!("Boss: {} ({})", assignment_label(a.kind), a.status.as_str());
    let description = a.notes.as_deref().unwrap_or("").to_string();
    out.push_str("BEGIN:VEVENT\r\n");
    out.push_str(&format!("UID:assign-{}@boss\r\n", a.id));
    out.push_str(&format!("DTSTAMP:{}\r\n", format_utc(now)));
    out.push_str(&format!("DTSTART:{}\r\n", format_utc(a.starts_at)));
    out.push_str(&format!("DTEND:{}\r\n", format_utc(a.ends_at)));
    out.push_str(&format!("SUMMARY:{}\r\n", escape_text(&summary)));
    if !description.is_empty() {
        out.push_str(&format!("DESCRIPTION:{}\r\n", escape_text(&description)));
    }
    // Cancelled / no-show assignments still get emitted so a tech can see
    // the history, but we flag them as TRANSP:TRANSPARENT (non-blocking).
    let transparent = matches!(
        a.status,
        AssignmentStatus::Cancelled | AssignmentStatus::NoShow
    );
    out.push_str(if transparent {
        "TRANSP:TRANSPARENT\r\n"
    } else {
        "TRANSP:OPAQUE\r\n"
    });
    out.push_str("END:VEVENT\r\n");
}

fn write_availability(out: &mut String, av: &TechAvailability, now: DateTime<Utc>) {
    let summary = format!("Boss: {}", availability_label(av.kind));
    out.push_str("BEGIN:VEVENT\r\n");
    out.push_str(&format!("UID:avail-{}@boss\r\n", av.id));
    out.push_str(&format!("DTSTAMP:{}\r\n", format_utc(now)));
    out.push_str(&format!("DTSTART:{}\r\n", format_utc(av.starts_at)));
    out.push_str(&format!("DTEND:{}\r\n", format_utc(av.ends_at)));
    out.push_str(&format!("SUMMARY:{}\r\n", escape_text(&summary)));
    if let Some(notes) = av.notes.as_deref()
        && !notes.is_empty()
    {
        out.push_str(&format!("DESCRIPTION:{}\r\n", escape_text(notes)));
    }
    out.push_str("TRANSP:OPAQUE\r\n");
    out.push_str("END:VEVENT\r\n");
}

fn assignment_label(kind: AssignmentKind) -> &'static str {
    match kind {
        AssignmentKind::Wo => "Service call",
        AssignmentKind::PreventiveMaintenance => "Preventive maintenance visit",
        AssignmentKind::Training => "Training",
        AssignmentKind::DiagCall => "Diagnostic call",
        AssignmentKind::Travel => "Travel",
        AssignmentKind::Install => "Install",
    }
}

fn availability_label(kind: AvailabilityKind) -> &'static str {
    match kind {
        AvailabilityKind::Available => "Working",
        AvailabilityKind::Pto => "PTO",
        AvailabilityKind::Sick => "Sick",
        AvailabilityKind::Holiday => "Holiday",
        AvailabilityKind::Training => "Training",
        AvailabilityKind::Blocked => "Blocked",
    }
}

/// RFC 5545 UTC form: `19970714T173000Z`.
fn format_utc(ts: DateTime<Utc>) -> String {
    ts.format("%Y%m%dT%H%M%SZ").to_string()
}

/// ICS TEXT-value escape: backslash, comma, semicolon get prefixed;
/// newlines become `\n`. See RFC 5545 §3.3.11.
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            '\n' => out.push_str("\\n"),
            '\r' => {} // strip — newlines are \n-encoded
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use uuid::Uuid;

    fn ts(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, mi, 0).unwrap()
    }

    fn test_assignment(kind: AssignmentKind, status: AssignmentStatus) -> ScheduledAssignment {
        ScheduledAssignment {
            id: Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888),
            tech_id: "emp-tech-1".into(),
            target_job_id: Uuid::nil(),
            kind,
            starts_at: ts(2026, 5, 1, 15, 0),
            ends_at: ts(2026, 5, 1, 17, 0),
            status,
            notes: Some("Bring beam profiler".into()),
            created_at: ts(2026, 4, 20, 0, 0),
            updated_at: ts(2026, 4, 20, 0, 0),
        }
    }

    fn test_pto() -> TechAvailability {
        TechAvailability {
            id: Uuid::from_u128(0xaaaa_bbbb_cccc_dddd_eeee_ffff_0000_1111),
            employee_id: "emp-tech-1".into(),
            kind: AvailabilityKind::Pto,
            starts_at: ts(2026, 5, 5, 0, 0),
            ends_at: ts(2026, 5, 10, 0, 0),
            notes: None,
            source: super::super::types::AvailabilitySource::Manual,
            created_at: ts(2026, 4, 1, 0, 0),
        }
    }

    #[test]
    fn ics_structure_is_valid_enough() {
        let now = ts(2026, 4, 22, 12, 0);
        let out = build_ics(
            "emp-tech-1",
            &[test_assignment(
                AssignmentKind::PreventiveMaintenance,
                AssignmentStatus::Confirmed,
            )],
            &[test_pto()],
            now,
        );
        assert!(out.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(out.ends_with("END:VCALENDAR\r\n"));
        assert!(out.contains("PRODID:-//Boss//tech/emp-tech-1//EN"));
        assert!(out.contains("BEGIN:VEVENT\r\n"));
        assert!(out.contains("SUMMARY:Boss: Preventive maintenance visit (confirmed)"));
        assert!(out.contains("SUMMARY:Boss: PTO"));
        assert!(out.contains("DTSTART:20260501T150000Z"));
        assert!(out.contains("DTEND:20260510T000000Z"));
    }

    #[test]
    fn available_slots_are_not_surfaced_as_events() {
        // "Available" is the working-hours signal; we don't spam the
        // tech's personal calendar with "You're at work" blocks.
        let now = ts(2026, 4, 22, 12, 0);
        let mut slot = test_pto();
        slot.kind = AvailabilityKind::Available;
        let out = build_ics("emp-tech-1", &[], &[slot], now);
        assert!(!out.contains("BEGIN:VEVENT"));
    }

    #[test]
    fn cancelled_assignment_emitted_transparent() {
        let now = ts(2026, 4, 22, 12, 0);
        let out = build_ics(
            "emp-tech-1",
            &[test_assignment(
                AssignmentKind::Wo,
                AssignmentStatus::Cancelled,
            )],
            &[],
            now,
        );
        assert!(out.contains("TRANSP:TRANSPARENT"));
        assert!(out.contains("SUMMARY:Boss: Service call (cancelled)"));
    }

    #[test]
    fn special_characters_escaped() {
        // Commas, semicolons, backslashes, newlines must escape.
        let now = ts(2026, 4, 22, 12, 0);
        let mut a = test_assignment(
            AssignmentKind::PreventiveMaintenance,
            AssignmentStatus::Confirmed,
        );
        a.notes = Some("one; two, three\\four\nfive".into());
        let out = build_ics("emp-tech-1", &[a], &[], now);
        assert!(out.contains("DESCRIPTION:one\\; two\\, three\\\\four\\nfive"));
    }
}
