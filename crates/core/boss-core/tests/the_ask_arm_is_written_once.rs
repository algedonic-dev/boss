//! The line that records an ask appears ONCE in `machine_gate.rs`, and
//! this file is where it may be quoted (backlog d653e090; review
//! 02e21a45 of car 176d5c9d, N3).
//!
//! Car 176d5c9d's recorded proof probe counts that line in the converged
//! `machine_gate.rs` with `grep -cF` and answers FAILED for any count
//! but one. A recorded probe is immutable and that one is re-run hourly
//! until a rotation asks — read 2026-10-08 it had answered `not yet` 28
//! times — so a later car quoting the line in a comment, a doc or a
//! unit test of that file would turn a correct car FAILED, on the forge,
//! an hour after it landed. Nothing in the tree said so: the constraint
//! lived in a packet. It lives here now, where the gate reads it, and it
//! fails naming the count.
//!
//! This is a holding action for one probe. When car 176d5c9d is proven
//! and nothing re-runs its probe, delete this file.

const GATE: &str = include_str!("../src/machine_gate.rs");
const ARM: &str = "gate.record(&req, Presented::Asked)";

#[test]
fn the_line_that_records_an_ask_is_written_once() {
    let n = GATE.matches(ARM).count();
    assert_eq!(
        n, 1,
        "machine_gate.rs holds `{ARM}` {n} time(s). Car 176d5c9d's recorded probe greps for \
         it and fails on any count but one: spell a second mention differently (a test \
         calls `g.record(...)`, prose can say \"the ask arm\"), never by quoting the line"
    );
}
