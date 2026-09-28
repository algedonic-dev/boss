//! The production binary signs its calendar client as the jobs service
//! (backlog 11721a25, 2026-09-27).
//!
//! The calendar asks policy of every reservation write since this car,
//! and a client that sends no identity is the headerless `anonymous`
//! caller, which no grant names. The step hook's reserve and cancel
//! would then be refused on every step start carrying a schedule — and
//! the hook runs only in the binary, whose `main` has no test. This
//! reads the binary's source, because the wiring is the one place a
//! port-level test cannot reach (the people API's pin does the same).

const BINARY: &str = include_str!("../src/bin/boss_jobs_api.rs");

#[test]
fn every_calendar_client_the_jobs_api_builds_is_signed() {
    let built: Vec<&str> = BINARY
        .split("ReqwestCalendarClient::new(")
        .skip(1)
        .map(|rest| rest.split(';').next().unwrap_or(rest))
        .collect();
    assert!(
        !built.is_empty(),
        "boss_jobs_api.rs no longer builds a ReqwestCalendarClient — move this pin with it"
    );
    for call in built {
        assert!(
            call.contains(".signed_as(\"automation:jobs\")"),
            "an unsigned calendar client in boss_jobs_api.rs: {call}"
        );
    }
}
