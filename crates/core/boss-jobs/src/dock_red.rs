//! A CAR WHOSE DOCK RE-GATE WENT RED IS NOT BOARDABLE, AND SAYS SO — the
//! one reading of that fact, for the conductor that writes it and the
//! surfaces that draw it (backlog 2fccbfd6).
//!
//! THE MEASURED CASE. On 2026-09-27 four released, green-parked cars sat
//! on the dock three to four hours while trains departed. The dock had
//! replayed each onto main and re-gated it (`boss-cli`'s `dock_regate`)
//! at 11:25-11:50Z, during a live-rules lint regression, and three of
//! those re-gates went red on that lint (gate-runs e77c4e30, af9ba752,
//! 48490860). The conductor held each car on its red — and nothing else
//! read the red: `boss orient` printed "HELD — none: every car on the
//! dock can board", the yard drew all four in its boardable lane, and no
//! alarm fired. A troubled packet must look troubled (CLAUDE.md
//! §Diagnosis), and here the only record of the trouble was prose in a
//! `skip_reason` no reader parses.
//!
//! SO THE RED IS DATA, ON THE STAMP THE DOCK ALREADY WRITES. The dock's
//! re-gate stamp (`base_regate`) carries a `red` object while the red
//! stands — the gate-run, the head and main it judged, its verdict word,
//! the checks its receipt named, and whether the dock has stopped
//! retrying it at this head (`garaged`). The conductor is its only writer and clears
//! it the pass the car's receipt vouches for its head again; the yard
//! (`yard::dock_lanes`) and `boss orient` read it here, so the three
//! cannot phrase or key it differently (CLAUDE.md §9a).

use serde_json::{Value, json};

/// The job-metadata key of the dock's re-gate stamp on a car. `boss-cli`
/// writes the whole object; this crate reads only its `red`.
pub const BASE_REGATE: &str = "base_regate";

/// Inside [`BASE_REGATE`]: the red the dock read for the head it
/// replayed, present only while that red stands.
pub const RED: &str = "red";

/// A dock re-gate that went red, as the car records it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegateRed {
    /// The gate-run that judged it.
    pub gate_run: String,
    /// The head it judged — the dock's replay of the car.
    pub head: String,
    /// The main that head was replayed onto.
    pub main: String,
    /// The gate-run's verdict word (`failed`, `lost`, `refused`).
    pub verdict: String,
    /// The checks its receipt named as failed, in the receipt's order.
    /// Empty when it named none (a lost run, a refusal).
    pub checks: Vec<String>,
    /// A retry went red too: the dock no longer re-gates this head, and
    /// re-gates the car only when main moves again. Likely the car's own,
    /// but not proven so — a regression on main reddens every car.
    pub garaged: bool,
}

fn short(s: &str) -> &str {
    &s[..8.min(s.len())]
}

impl RegateRed {
    pub fn to_value(&self) -> Value {
        json!({
            "gate_run": self.gate_run,
            "head": self.head,
            "main": self.main,
            "verdict": self.verdict,
            "checks": self.checks,
            "garaged": self.garaged,
        })
    }

    /// The failing checks as one phrase, or the fact that none was named
    /// — a lost run and a refusal name none, and saying so is the answer.
    fn checks_phrase(&self) -> String {
        if self.checks.is_empty() {
            "no check named".to_string()
        } else {
            self.checks.join(", ")
        }
    }

    /// The one sentence every surface prints for this car — the
    /// conductor's `skip_reason`, the yard's held lane and `boss orient`.
    pub fn reason(&self) -> String {
        let what = format!(
            "its dock re-gate {} went {} on {} (head {} on main {})",
            short(&self.gate_run),
            self.verdict,
            self.checks_phrase(),
            short(&self.head),
            short(&self.main),
        );
        if self.garaged {
            // Two reds in a row do not prove the red is the car's: a
            // regression on main reddens every car it touches, across a
            // main move too (the adversarial review of car 5eb1967e). So
            // the sentence names what would prove it rather than claiming
            // it, and says what the dock still does.
            format!(
                "NOT boardable, garaged: {what}, its second dock re-gate in a row to go red. The \
                 dock does not re-gate it again at this head; it re-gates it once each time main \
                 moves, because a red main shares is not the car's. If main's own gate is green \
                 on the same check, the red is the car's: its builder repairs it and re-gates \
                 with boss gate --rebase --park-*, which refreshes this car (backlog 2fccbfd6)"
            )
        } else {
            format!(
                "NOT boardable: {what}. The dock re-gates it again on the next main move, or \
                 after its bound if main does not move; a second red garages it at this head \
                 (backlog 2fccbfd6)"
            )
        }
    }
}

/// The standing red on a car's dock re-gate stamp, read from the car's
/// METADATA object. `None` when the stamp carries none — or a `red` that
/// names no gate-run, which nobody could act on.
pub fn regate_red(metadata: &Value) -> Option<RegateRed> {
    let red = metadata.get(BASE_REGATE)?.get(RED)?.as_object()?;
    let text = |k: &str| {
        red.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let gate_run = text("gate_run");
    if gate_run.is_empty() {
        return None;
    }
    Some(RegateRed {
        gate_run,
        head: text("head"),
        main: text("main"),
        verdict: text("verdict"),
        checks: red
            .get("checks")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        garaged: red.get("garaged").and_then(Value::as_bool).unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn red() -> RegateRed {
        RegateRed {
            gate_run: "e77c4e30-349a-4dc5-844f-8117fe63b12d".into(),
            head: "51cac5a1c0b5455defeec10c8ea344e1ee51fe97".into(),
            main: "620603309547562e3d27bde4433fa39cbd4ae6e5".into(),
            verdict: "failed".into(),
            checks: vec!["test".into(), "web-suite (unit+build+mocked)".into()],
            garaged: false,
        }
    }

    /// Car 51ad323d as it stood at 15:10Z on 2026-09-27: its stamp's red
    /// round-trips through the metadata the conductor writes, and a stamp
    /// with no red — every re-gate in flight or green — reads as none.
    #[test]
    fn the_red_round_trips_through_the_stamp() {
        let md = json!({ BASE_REGATE: {
            "main": "620603309547562e3d27bde4433fa39cbd4ae6e5",
            "head": "51cac5a1c0b5455defeec10c8ea344e1ee51fe97",
            "gate_run": "e77c4e30-349a-4dc5-844f-8117fe63b12d",
            "missed": 1,
            RED: red().to_value(),
        }});
        assert_eq!(regate_red(&md), Some(red()));
        let running = json!({ BASE_REGATE: { "main": "m", "head": "h", "gate_run": "g" } });
        assert_eq!(regate_red(&running), None, "in flight is not red");
        assert_eq!(regate_red(&json!({})), None);
        let nulled = json!({ BASE_REGATE: { "main": "m", RED: null } });
        assert_eq!(regate_red(&nulled), None, "a cleared red is no red");
        let blank = json!({ BASE_REGATE: { "main": "m", RED: { "gate_run": "" } } });
        assert_eq!(
            regate_red(&blank),
            None,
            "a red that names no gate-run cannot be acted on, so it is not one"
        );
    }

    /// The sentence names what an operator acts on: the gate-run, the
    /// failing check, the head and main, and what happens next — a retry
    /// while the dock still owns it, the builder's repair once garaged.
    #[test]
    fn the_reason_names_the_run_the_check_and_what_happens_next() {
        let line = red().reason();
        for want in [
            "e77c4e30",
            "test",
            "web-suite (unit+build+mocked)",
            "51cac5a1",
            "62060330",
            "NOT boardable",
            "re-gates it again",
        ] {
            assert!(line.contains(want), "{want}: {line}");
        }
        let garaged = RegateRed {
            garaged: true,
            ..red()
        }
        .reason();
        for want in [
            "garaged",
            "e77c4e30",
            "test",
            "--park-",
            "each time main moves",
            "main's own gate",
        ] {
            assert!(garaged.contains(want), "{want}: {garaged}");
        }
        assert!(!garaged.contains("re-gates it again"), "{garaged}");
        // Two reds in a row do not prove the red is the car's: the
        // measured incident's lint regression reddened every car across
        // a main move (the adversarial review of car 5eb1967e). The
        // sentence says what would prove it instead of asserting it.
        assert!(!garaged.contains("is the car's own"), "{garaged}");
        let unnamed = RegateRed {
            verdict: "lost".into(),
            checks: vec![],
            ..red()
        }
        .reason();
        assert!(
            unnamed.contains("lost") && unnamed.contains("no check named"),
            "{unnamed}"
        );
    }
}
