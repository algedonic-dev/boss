//! The production binary signs its calendar client as the people
//! service (backlog 11721a25, 2026-09-27).
//!
//! The calendar asks policy of every reservation write since this car,
//! and a client that sends no identity is the headerless `anonymous`
//! caller, which no grant names: every PTO booking would be refused at
//! the calendar after passing its own gate. The wiring lives in the
//! binary, whose `main` has no test, so this reads its source.

const BINARY: &str = include_str!("../src/bin/boss_people_api.rs");

#[test]
fn every_calendar_client_the_people_api_builds_is_signed() {
    let built: Vec<&str> = BINARY
        .split("ReqwestCalendarClient::new(")
        .skip(1)
        .map(|rest| rest.split(';').next().unwrap_or(rest))
        .collect();
    assert!(
        !built.is_empty(),
        "boss_people_api.rs no longer builds a ReqwestCalendarClient — move this pin with it"
    );
    for call in built {
        assert!(
            call.contains(".signed_as(\"automation:people\")"),
            "an unsigned calendar client in boss_people_api.rs: {call}"
        );
    }
}
