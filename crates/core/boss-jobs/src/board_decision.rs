//! What the conductor's board decided on its tick — ONE definition, read by
//! the conductor that decides it and by the yard, `boss orient` and the
//! NEXT UP row that report it (backlog 96f02540).
//!
//! WHY IT LIVES HERE. From 02:42Z to past 03:15Z on 2026-09-28 the board
//! refused on every 60-second tick with seven cars on the dock — it was
//! holding for the dock's re-gate round — while `boss orient` printed
//! "train-board due now — nothing holds it". The yard re-derived the
//! board's decision from the cadence rows alone (the track, the cooldown,
//! the depth), and the board's own refusals lived only in the conductor's
//! journal, where nothing that draws the yard could read them. A troubled
//! packet must look troubled (CLAUDE.md §Diagnosis), and a surface that
//! re-decides what another component decided will, sooner or later,
//! decide differently.
//!
//! So the refusal the board journals ([`NoDeparture`], its line, and
//! whether it clears itself) moved here from `boss-cli`, the way the
//! ordering edge's judgement did before it (`car::boards_after_outcome`,
//! backlog 4142d821); every board tick RECORDS its decision
//! ([`BoardDecision`]) on the cadence firing that ran it, under
//! [`FIRING_KEY`]; and the yard's boarding hold reads that record rather
//! than guessing (`yard::boarding_hold`). Nothing here reads a clock or a
//! store: the conductor decides, the firing carries it, the yard states it.

use serde::{Deserialize, Serialize};

/// The key a board firing's `detail` carries its decision under — merged
/// in with the outcome (`cadence::FiringOutcome::board_decision`).
pub const FIRING_KEY: &str = "board_decision";

// ---------------------------------------------------------------------------
// A BOARD THAT DEPARTS NO TRAIN OPENS NO PACKET (backlog 4860aff8) — the
// reasons it gives instead. The history of each variant is on the
// conductor's side (`boss-cli/src/train/boarding.rs`); the words are here
// so the reader that reports them is the reader that wrote them.
// ---------------------------------------------------------------------------

/// Why a board attempt departed no train. None of these opens a packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoDeparture {
    /// A train before its merge holds the one track (`occupant` names it).
    /// Checked first, before anything is collected.
    TrackOccupied { occupant: String },
    /// The CI host is short of what a run needs — an infrastructure
    /// refusal, decided before a single car was collected.
    HostShort { reason: String },
    /// Nothing was parked and ready when the window opened.
    NothingParked,
    /// Every candidate conflicted on the assembled tree.
    AllConflicted { branches: String },
    /// The consist check refused the assembled tree. Nobody's car is at
    /// fault — each was green on its own branch — so every car stays
    /// boardable and unstruck.
    ConsistRefused { reason: String, cars: usize },
    /// Every car on the dock is held on a declared ordering edge
    /// (`metadata.boards_after`) or garaged on a red dock re-gate.
    ///
    /// TWO LISTS, BECAUSE THEY ASK DIFFERENT THINGS OF THE READER. A car
    /// waiting on a predecessor still in flight needs nobody; a car whose
    /// predecessor was abandoned, whose edge names no Job, or whose red is
    /// no longer re-gated, can never depart until a person clears it
    /// (d3320278).
    HeldOnEdges { cars: String, needs_human: String },
}

/// Will this refusal still be here on the next window, unchanged?
///
/// WHY THE DISTINCTION IS THE WHOLE ALARM (backlog 6baabd43). From
/// 04:27Z to 13:49Z on one day no train departed. The conductor never
/// stopped and never failed: it fired every minute, took its lock, ran
/// preflight, evaluated all three parked cars and logged in full —
/// naming all three branches and the conflicting files for each. Nine
/// and a half hours of perfect diagnosis with zero reach: no packet, no
/// alarm, no surface.
///
/// AND AN ALARM ON "NO TRAIN DEPARTED" ALONE WOULD BE NOISE. Most
/// windows refuse for reasons that clear themselves within a minute —
/// an idle dock, a car waiting on a predecessor still in flight. So the
/// signal is not "nothing departed"; it is "nothing departed FOR A
/// REASON THAT WILL NOT CLEAR ITSELF". A total match, not a heuristic:
///
/// - `TrackOccupied` — the train on the track merges or is cancelled.
/// - `NothingParked` — an idle window. The next parked car departs.
/// - `HeldOnEdges` with nobody needing a human — each car boards by
///   itself once the car it named has landed.
/// - `HostShort` — an infrastructure refusal that clears when the host
///   does, and which says nothing about any branch.
///
/// against the three that repeat identically until a person acts:
///
/// - `AllConflicted` — every candidate conflicts on the assembled tree,
///   and will again on the next window, and the next.
/// - `ConsistRefused` — the assembled tree is refused; nobody's car is
///   at fault and nothing on the dock can change it.
/// - `HeldOnEdges` with `needs_human` — an edge that can never be
///   satisfied; the window refuses identically forever.
///
/// (`AwaitingRegates`, the departure that waited for the dock's re-gate
/// round, is gone with the round itself — backlog 96f02540: the board
/// never waits on a re-gate.)
pub fn refusal_persists(refusal: &NoDeparture) -> bool {
    match refusal {
        NoDeparture::TrackOccupied { .. }
        | NoDeparture::NothingParked
        | NoDeparture::HostShort { .. } => false,
        NoDeparture::HeldOnEdges { needs_human, .. } => !needs_human.is_empty(),
        NoDeparture::AllConflicted { .. } | NoDeparture::ConsistRefused { .. } => true,
    }
}

/// The journal line a refused board leaves — and, through
/// [`BoardDecision`], the sentence the yard states. It carries the reason
/// AND the fact that no packet was opened; a reader who greps `no train
/// departed` gets every non-departure but the track's, which keeps the
/// line it has always had.
pub fn no_departure_line(refusal: &NoDeparture) -> String {
    match refusal {
        NoDeparture::TrackOccupied { occupant } => {
            format!("BOARDING HELD — track occupied by {occupant}")
        }
        NoDeparture::HostShort { reason } => format!(
            "no train departed — boarding refused before any car was collected: {reason}. \
             No train packet opened, no PR, no CI spent."
        ),
        NoDeparture::NothingParked => "no train departed — no car was parked and ready when the \
             window opened: an idle window, not a failure. No train packet opened."
            .to_string(),
        NoDeparture::AllConflicted { branches } => format!(
            "no train departed — every candidate was skipped on merge conflicts: {branches}. \
             No train packet opened, no PR, no CI spent; each car carries its own skip_reason."
        ),
        NoDeparture::ConsistRefused { reason, cars } => format!(
            "no train departed — {reason}. No train packet opened, no PR, no CI spent — \
             {cars} car(s) stay boardable and unstruck."
        ),
        NoDeparture::HeldOnEdges { cars, needs_human } if needs_human.is_empty() => format!(
            "no train departed — every car on the dock is held on the ordering edge it \
             declared: {cars}. No train packet opened. NOTHING NEEDS DOING: each boards by \
             itself on a later window, once the car it named has landed."
        ),
        NoDeparture::HeldOnEdges { cars, needs_human } => format!(
            "no train departed — every car on the dock is held, on the ordering edge it \
             declared or garaged on a red dock re-gate: {cars}. No train packet opened. A \
             HUMAN IS NEEDED for {needs_human}: that edge can never be satisfied, or that red \
             is not re-gated again at its head, so this window will refuse identically until \
             someone clears it — each car's own skip_reason names which."
        ),
    }
}

/// What one board tick decided — recorded on the cadence firing that ran
/// it, so every reader of "when does the next train board" reads the
/// board's own answer rather than re-deriving one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "kebab-case")]
pub enum BoardDecision {
    /// A train departed carrying `cars`.
    Boarded { cars: usize },
    /// The board refused whatever the dock held: the track, the CI host,
    /// the consist check.
    Held { reason: String },
    /// Cars were asked, and none could board: why.
    NoBoardableCar { reason: String },
}

impl From<&NoDeparture> for BoardDecision {
    fn from(refusal: &NoDeparture) -> Self {
        let reason = no_departure_line(refusal);
        match refusal {
            NoDeparture::TrackOccupied { .. }
            | NoDeparture::HostShort { .. }
            | NoDeparture::ConsistRefused { .. } => BoardDecision::Held { reason },
            NoDeparture::NothingParked
            | NoDeparture::AllConflicted { .. }
            | NoDeparture::HeldOnEdges { .. } => BoardDecision::NoBoardableCar { reason },
        }
    }
}

impl BoardDecision {
    /// The decision as one line: `boarded N car(s)`, `held: <reason>`, or
    /// `no boardable car: <reason>`.
    pub fn line(&self) -> String {
        match self {
            BoardDecision::Boarded { cars } => format!("boarded {cars} car(s)"),
            BoardDecision::Held { reason } => format!("held: {reason}"),
            BoardDecision::NoBoardableCar { reason } => format!("no boardable car: {reason}"),
        }
    }

    /// The refusal this decision states, as the yard's `held_because`
    /// carries it — `None` for a departure.
    pub fn refusal(&self) -> Option<String> {
        match self {
            BoardDecision::Boarded { .. } => None,
            _ => Some(self.line()),
        }
    }

    /// Read a firing's decision out of its `detail` — `None` when it
    /// carries none (no outcome yet, a verb that is not a board, a firing
    /// from before 96f02540) or one this build cannot parse.
    pub fn of_detail(detail: &serde_json::Value) -> Option<Self> {
        detail
            .get(FIRING_KEY)
            .filter(|v| !v.is_null())
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn every_refusal() -> Vec<NoDeparture> {
        vec![
            NoDeparture::TrackOccupied {
                occupant: "train/20260928-0240".into(),
            },
            NoDeparture::HostShort {
                reason: "forge: 3.1 GB free, floor is 20 GB".into(),
            },
            NoDeparture::NothingParked,
            NoDeparture::AllConflicted {
                branches: "fix/a, fix/b".into(),
            },
            NoDeparture::ConsistRefused {
                reason: "consist check refused — two rule cars".into(),
                cars: 3,
            },
            NoDeparture::HeldOnEdges {
                cars: "aaaaaaaa".into(),
                needs_human: String::new(),
            },
            NoDeparture::HeldOnEdges {
                cars: "aaaaaaaa, cccccccc".into(),
                needs_human: "cccccccc".into(),
            },
        ]
    }

    /// Every refusal becomes a decision that REFUSES, and its sentence is
    /// the conductor's own journal line, word for word — so the yard can
    /// never state a softer reason than the board gave.
    #[test]
    fn every_refusal_is_a_decision_that_says_the_journals_words() {
        for r in every_refusal() {
            let d = BoardDecision::from(&r);
            let said = d.refusal().expect("a refusal refuses");
            assert!(
                said.ends_with(&no_departure_line(&r)),
                "the decision carries the journal line verbatim: {said}"
            );
        }
        assert_eq!(BoardDecision::Boarded { cars: 3 }.refusal(), None);
        assert_eq!(
            BoardDecision::Boarded { cars: 3 }.line(),
            "boarded 3 car(s)"
        );
    }

    /// Held versus no-boardable-car is the reader's first question: is the
    /// board refusing the dock, or is the dock giving it nothing?
    #[test]
    fn a_refusal_of_the_window_is_held_and_an_empty_dock_is_not() {
        let held = |r: NoDeparture| matches!(BoardDecision::from(&r), BoardDecision::Held { .. });
        assert!(held(NoDeparture::TrackOccupied {
            occupant: "t".into()
        }));
        assert!(held(NoDeparture::HostShort { reason: "x".into() }));
        assert!(held(NoDeparture::ConsistRefused {
            reason: "x".into(),
            cars: 1
        }));
        assert!(!held(NoDeparture::NothingParked));
        assert!(!held(NoDeparture::AllConflicted {
            branches: "a".into()
        }));
        assert!(!held(NoDeparture::HeldOnEdges {
            cars: "a".into(),
            needs_human: String::new()
        }));
    }

    /// The record round-trips through a firing's `detail`, beside what the
    /// claim and the outcome already put there, and a firing that carries
    /// none — or one this build cannot read — reads as none.
    #[test]
    fn the_decision_round_trips_through_a_firings_detail() {
        for d in every_refusal()
            .iter()
            .map(BoardDecision::from)
            .chain([BoardDecision::Boarded { cars: 2 }])
        {
            let detail = json!({"dock_depth": 7, "rc": -2, FIRING_KEY: d});
            assert_eq!(BoardDecision::of_detail(&detail), Some(d));
        }
        assert_eq!(BoardDecision::of_detail(&json!({"rc": 0})), None);
        assert_eq!(BoardDecision::of_detail(&json!({FIRING_KEY: null})), None);
        assert_eq!(
            BoardDecision::of_detail(&json!({FIRING_KEY: {"decision": "teleported"}})),
            None
        );
        assert_eq!(
            serde_json::to_value(BoardDecision::NoBoardableCar { reason: "r".into() }).unwrap(),
            json!({"decision": "no-boardable-car", "reason": "r"}),
            "the wire shape a surface reads"
        );
    }

    /// The track's line is the one the conductor has always journalled,
    /// and a held track clears itself, like an idle window.
    #[test]
    fn the_track_keeps_its_line_and_is_no_stall() {
        let r = NoDeparture::TrackOccupied {
            occupant: "train/20260928-0240".into(),
        };
        assert_eq!(
            no_departure_line(&r),
            "BOARDING HELD — track occupied by train/20260928-0240"
        );
        assert!(!refusal_persists(&r));
    }
}
