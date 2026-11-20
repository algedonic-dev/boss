//! THE ORDER OF THE GATE LINE (backlog 64d960e7).
//!
//! Until 2026-11-19 the resident line was first come, first served: the
//! supervisor walked its accepted requests by original acceptance alone.
//! Measured that day over the 60 newest closed gate-runs, the gates
//! station was used for about 33 of its 54 capacity-hours, yet cars
//! waited up to 82 min in bursts (six builders handed off between 16:03
//! and 16:46), and the urgent standby car's re-gate (70d16d8a, car
//! 5a1cbc09, item 47601829) accepted at 14:37:31 waited behind two
//! standard first gates accepted at 13:30:16 and 14:24:02, launching at
//! 15:07:54 rather than into the gate that freed at 14:52:13.
//!
//! What stands AHEAD of this line is unchanged and is not decided here:
//! a waiting train's claim and the dock's re-gate claims (`admits`,
//! `trains_waiting`, `dock_waiting`), and main's full gate still yields
//! to every queued car (`boss_jobs::full_gate_on_main`). This module
//! orders the requests BEHIND those claims:
//!
//!  1. urgent (or emergency) work — the item the gate answers, the car
//!     it carries, or the gate-run itself — and anything that has waited
//!     past [`AGE_CAP_SECS`], so a first gate can never starve;
//!  2. a car already released at the dock, whose next stop is a train
//!     (`train::parked_ready`: parked, review open, no hold);
//!  3. everything else — first gates and cars still under review — by
//!     age.
//!
//! Within a tier the order is the one it always was: original acceptance,
//! then packet id.
use serde_json::Value;

/// How long a request may wait before it joins the front tier whatever
/// it is. The measured worst wait on 2026-11-19 was 82 min, inside a
/// burst; two hours is past any burst that day yet short of a starving
/// gate, and a cap is what makes the priority tiers safe to have at all.
pub(crate) const AGE_CAP_SECS: i64 = 2 * 3600;

/// The tier a request stands in. Ordered: the derive compares in
/// declaration order, so `Front` sorts first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Tier {
    /// Urgent or emergency work, or a request past [`AGE_CAP_SECS`].
    Front,
    /// A car released at the dock: its next stop is a train.
    Released,
    /// A first gate, or a car not yet released, by age.
    Arrival,
}

/// Where a request stands and why, in words its packet can carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Standing {
    pub tier: Tier,
    pub why: String,
}

/// PURE: the item a gate answers — its own park answer first, which the
/// builder gave with the finished diff, else the item its open car
/// carries (`train::priority_item`). `None` for a gate that names none.
pub(crate) fn answered_item<'a>(gate: &'a Value, car: Option<&'a Value>) -> Option<&'a str> {
    let md = gate.get("metadata");
    [
        boss_jobs::car::PARK_BACKLOG_ITEM,
        boss_jobs::car::PARK_PARTIAL_ITEM,
    ]
    .into_iter()
    .find_map(|key| {
        md.and_then(|m| m.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    })
    .or_else(|| car.and_then(crate::train::priority_item))
}

/// The open car a gate's branch carries, as `boss gate`'s own guard
/// finds it (`car::open_car_for`).
pub(crate) fn car_of<'a>(gate: &Value, cars: &'a [Value]) -> Option<&'a Value> {
    gate.pointer("/metadata/branch")
        .and_then(Value::as_str)
        .filter(|b| !b.is_empty())
        .and_then(|branch| boss_jobs::car::open_car_for(cars, branch))
}

/// The drain rank's word: 0 is emergency, 1 urgent.
fn pressing_word(rank: u8) -> &'static str {
    if rank == 0 { "emergency" } else { "urgent" }
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// PURE: where one request stands. `ranks` maps an item id to its drain
/// rank (`top_board::priority_rank`: 0 emergency, 1 urgent, 2 otherwise);
/// an item missing from it ranks standard, because ordering decides who
/// goes first, never who goes.
pub(crate) fn standing(
    gate: &Value,
    cars: &[Value],
    ranks: &std::collections::HashMap<String, u8>,
    accepted_at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> Standing {
    let car = car_of(gate, cars);
    let pressing = |rank: u8| (rank < 2).then_some(rank);
    let item = answered_item(gate, car);
    let car_id = car
        .and_then(|c| c.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    // The item first, because it is what the work is for; then the rows
    // themselves. The first of equal ranks is the one said.
    let said = [
        item.and_then(|i| {
            let r = ranks.get(i).copied().and_then(pressing)?;
            Some((r, format!("answers {} item {}", pressing_word(r), short(i))))
        }),
        pressing(crate::top_board::priority_rank(gate))
            .map(|r| (r, format!("the gate-run itself is {}", pressing_word(r)))),
        car.and_then(|c| pressing(crate::top_board::priority_rank(c)))
            .map(|r| {
                (
                    r,
                    format!("its car {} is {}", short(car_id), pressing_word(r)),
                )
            }),
    ]
    .into_iter()
    .flatten()
    .min_by_key(|(rank, _)| *rank);
    if let Some((rank, said)) = said {
        return Standing {
            tier: Tier::Front,
            why: format!("{}: {said}", pressing_word(rank)),
        };
    }
    let waited = (now - accepted_at).num_seconds();
    if waited > AGE_CAP_SECS {
        return Standing {
            tier: Tier::Front,
            why: format!(
                "waiting {} h {:02} min, past the {} h age cap: promoted so no gate starves",
                waited / 3600,
                (waited % 3600) / 60,
                AGE_CAP_SECS / 3600
            ),
        };
    }
    match car {
        Some(c) if crate::train::parked_ready(c) => Standing {
            tier: Tier::Released,
            why: format!(
                "car {} is released at the dock: its next stop is a train",
                short(car_id)
            ),
        },
        Some(c) if boss_jobs::car::is_parked(c) => Standing {
            tier: Tier::Arrival,
            why: format!("car {} is held at the dock, so by age", short(car_id)),
        },
        _ => Standing {
            tier: Tier::Arrival,
            why: "a first gate, by age".into(),
        },
    }
}

/// The ordering key: tier, then original acceptance, then packet id.
pub(crate) type Key<'a> = (Tier, chrono::DateTime<chrono::Utc>, &'a str);

/// PURE: how many legacy places stand before `key` in the gate line:
/// every place in a tier before it, and within its own tier the very
/// comparison a legacy waiter makes (`places_taken_before`, backlog
/// b56bfc29), so the two owners still agree about a same-tier pair. A
/// legacy waiter reads its own line by arrival alone; it is a reader the
/// resident cutover is retiring, and it is not reordered here.
pub(crate) fn legacy_ahead(
    legacy: &[(Tier, chrono::DateTime<chrono::Utc>, String)],
    key: Key<'_>,
) -> usize {
    let (tier, at, id) = key;
    let before = legacy.iter().filter(|(t, _, _)| *t < tier).count();
    let same: Vec<(chrono::DateTime<chrono::Utc>, String)> = legacy
        .iter()
        .filter(|(t, _, _)| *t == tier)
        .map(|(_, when, packet)| (*when, packet.clone()))
        .collect();
    before + crate::gate::places_taken_before(&same, at, id)
}

/// PURE: the sentence a waiting request carries on its packet.
pub(crate) fn place_text(
    position: usize,
    trains: usize,
    dock: usize,
    earlier: usize,
    why: &str,
) -> String {
    let count = |n: usize, one: &str, many: &str| {
        (n > 0).then(|| format!("{n} {}", if n == 1 { one } else { many }))
    };
    let parts: Vec<String> = [
        count(trains, "train gate", "train gates"),
        count(dock, "dock re-gate", "dock re-gates"),
        count(earlier, "gate ahead in line", "gates ahead in line"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let ahead = match parts.split_last() {
        None => "next for a free gate".to_string(),
        Some((last, [])) => format!("behind {last}"),
        Some((last, rest)) => format!("behind {} and {last}", rest.join(", ")),
    };
    format!("position {position}: {ahead}; {why}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// A resident gate-run as the supervisor reads it: the branch and the
    /// park answer the builder gave, nothing else that ordering reads.
    fn gate(id: &str, branch: &str, answer: (&str, &str)) -> Value {
        json!({"id": id, "priority": "standard",
            "metadata": {"branch": branch, answer.0: answer.1}})
    }

    /// A car as `kind=ship-a-change&status=open&full=true` returns it,
    /// with the review step the dock reads.
    fn car(branch: &str, item: &str, review: &str, review_md: Value) -> Value {
        json!({"id": format!("car-{branch}"), "priority": "standard", "status": "open",
            "metadata": {"branch": branch, "partial_item": item},
            "steps": [
                {"spec_slug": "gate", "title": "Gate", "status": "completed", "metadata": {}},
                {"spec_slug": "review", "title": "Open for review", "status": review,
                 "metadata": review_md}]})
    }

    fn ordered(
        accepted: &[(&str, &str)],
        gates: &[Value],
        cars: &[Value],
        ranks: &HashMap<String, u8>,
        now: &str,
    ) -> Vec<String> {
        let now = at(now);
        let mut keyed: Vec<(Tier, chrono::DateTime<chrono::Utc>, String)> = accepted
            .iter()
            .map(|(id, when)| {
                let row = gates.iter().find(|g| g["id"] == *id).unwrap();
                let s = standing(row, cars, ranks, at(when), now);
                (s.tier, at(when), id.to_string())
            })
            .collect();
        keyed.sort();
        keyed.into_iter().map(|(_, _, id)| id).collect()
    }

    /// TODAY'S CASE, from the record of 2026-11-19. Two standard first
    /// gates accepted at 13:30:16 and 14:24:02, then the urgent standby
    /// car's re-gate at 14:37:31 (answering urgent item 47601829 as a
    /// partial). At 14:52:13 a gate freed and FIFO gave it to 904bc3fd;
    /// the urgent re-gate goes first.
    #[test]
    fn the_urgent_standby_re_gate_goes_before_older_first_gates() {
        let gates = vec![
            gate(
                "904bc3fd",
                "fix/policy-decision-cache-sweeps-expired-and-caps-its-size",
                ("park_backlog_item", "5f83e4ed"),
            ),
            gate(
                "02a682c9",
                "fix/account-type-and-tier-checks-read-their-own-class-axis",
                ("park_partial_item", "3ff7205c"),
            ),
            gate(
                "70d16d8a",
                "feat/a-standby-shell-stands-off-the-dev-pods-node",
                ("park_partial_item", "47601829"),
            ),
        ];
        // The car was held for its re-review at the time: not released.
        let cars = vec![car(
            "feat/a-standby-shell-stands-off-the-dev-pods-node",
            "47601829",
            "active",
            json!({"hold": "waits for review run 051927d9"}),
        )];
        let ranks = HashMap::from([
            ("5f83e4ed".to_string(), 2),
            ("3ff7205c".to_string(), 2),
            ("47601829".to_string(), 1),
        ]);
        let accepted = [
            ("904bc3fd", "2026-11-19T13:30:16Z"),
            ("02a682c9", "2026-11-19T14:24:02Z"),
            ("70d16d8a", "2026-11-19T14:37:31Z"),
        ];
        assert_eq!(
            ordered(&accepted, &gates, &cars, &ranks, "2026-11-19T14:52:13Z"),
            vec!["70d16d8a", "904bc3fd", "02a682c9"]
        );
        let urgent = standing(
            &gates[2],
            &cars,
            &ranks,
            at("2026-11-19T14:37:31Z"),
            at("2026-11-19T14:52:13Z"),
        );
        assert_eq!(urgent.tier, Tier::Front);
        assert!(urgent.why.contains("urgent"), "{}", urgent.why);
        assert!(urgent.why.contains("47601829"), "{}", urgent.why);
    }

    /// The item a gate answers is its own park answer, else its car's.
    #[test]
    fn a_gate_answers_its_park_item_else_its_cars() {
        let parked = gate("g", "fix/x", ("park_backlog_item", "item-a"));
        let bare = json!({"id": "h", "metadata": {"branch": "fix/x"}});
        let c = car("fix/x", "item-b", "ready", json!({}));
        assert_eq!(answered_item(&parked, Some(&c)), Some("item-a"));
        assert_eq!(answered_item(&bare, Some(&c)), Some("item-b"));
        assert_eq!(answered_item(&bare, None), None);
    }

    /// A released car's re-gate — parked, review open, no hold — goes
    /// before an older first gate: its next stop is a train. A HELD car
    /// at the dock is not released and keeps its arrival place.
    #[test]
    fn a_released_cars_re_gate_goes_before_older_first_gates_and_a_held_one_does_not() {
        let gates = vec![
            gate("first", "fix/new-work", ("park_backlog_item", "i1")),
            gate("released", "fix/released", ("park_backlog_item", "i2")),
            gate("held", "fix/held", ("park_backlog_item", "i3")),
        ];
        let cars = vec![
            car("fix/released", "i2", "ready", json!({})),
            car(
                "fix/held",
                "i3",
                "ready",
                json!({"hold": "a person's hold"}),
            ),
        ];
        let ranks = HashMap::new();
        let accepted = [
            ("first", "2026-11-19T16:03:00Z"),
            ("held", "2026-11-19T16:04:00Z"),
            ("released", "2026-11-19T16:05:00Z"),
        ];
        assert_eq!(
            ordered(&accepted, &gates, &cars, &ranks, "2026-11-19T16:10:00Z"),
            vec!["released", "first", "held"]
        );
        let s = standing(
            &gates[1],
            &cars,
            &ranks,
            at("2026-11-19T16:05:00Z"),
            at("2026-11-19T16:10:00Z"),
        );
        assert_eq!(s.tier, Tier::Released);
        assert!(s.why.contains("next stop is a train"), "{}", s.why);
    }

    /// FAIR: a first gate past the age cap joins the front tier and, being
    /// older, goes before a fresher urgent one; one minute short of the
    /// cap it does not.
    #[test]
    fn a_first_gate_past_two_hours_is_promoted_ahead_of_fresher_urgent_work() {
        let gates = vec![
            gate("old", "fix/old", ("park_backlog_item", "plain")),
            gate("urgent", "fix/urgent", ("park_backlog_item", "hot")),
        ];
        let ranks = HashMap::from([("plain".to_string(), 2), ("hot".to_string(), 1)]);
        let accepted = [
            ("old", "2026-11-19T10:00:00Z"),
            ("urgent", "2026-11-19T11:30:00Z"),
        ];
        assert_eq!(
            ordered(&accepted, &gates, &[], &ranks, "2026-11-19T12:00:01Z"),
            vec!["old", "urgent"]
        );
        assert_eq!(
            ordered(&accepted, &gates, &[], &ranks, "2026-11-19T11:59:00Z"),
            vec!["urgent", "old"]
        );
        let aged = standing(
            &gates[0],
            &[],
            &ranks,
            at("2026-11-19T10:00:00Z"),
            at("2026-11-19T12:00:01Z"),
        );
        assert_eq!(aged.tier, Tier::Front);
        assert!(aged.why.contains("2 h"), "{}", aged.why);
    }

    /// An unread item is standard, never urgent; a gate-run or car marked
    /// urgent on its own row is urgent without an item.
    #[test]
    fn urgency_is_read_off_the_item_the_car_or_the_gate_and_an_unread_item_is_standard() {
        let now = at("2026-11-19T12:00:00Z");
        let then = at("2026-11-19T11:50:00Z");
        let unread = gate("u", "fix/u", ("park_backlog_item", "never-read"));
        assert_eq!(
            standing(&unread, &[], &HashMap::new(), then, now).tier,
            Tier::Arrival
        );
        let mut own = gate("o", "fix/o", ("park_backlog_item", "never-read"));
        own["priority"] = json!("urgent");
        assert_eq!(
            standing(&own, &[], &HashMap::new(), then, now).tier,
            Tier::Front
        );
        let mut hot_car = car("fix/c", "never-read", "active", json!({"hold": "review"}));
        hot_car["priority"] = json!("emergency");
        let bare = json!({"id": "c", "priority": "standard", "metadata": {"branch": "fix/c"}});
        let s = standing(&bare, &[hot_car], &HashMap::new(), then, now);
        assert_eq!(s.tier, Tier::Front);
        assert!(s.why.contains("emergency"), "{}", s.why);
    }

    /// Legacy places count ahead only when they would sort ahead: an
    /// urgent resident counts no older standard legacy place.
    #[test]
    fn a_legacy_place_counts_ahead_only_when_it_sorts_ahead() {
        let legacy = vec![
            (Tier::Arrival, at("2026-11-19T13:00:00Z"), "l1".to_string()),
            (Tier::Arrival, at("2026-11-19T15:00:00Z"), "l2".to_string()),
        ];
        let when = at("2026-11-19T14:00:00Z");
        assert_eq!(legacy_ahead(&legacy, (Tier::Front, when, "r")), 0);
        assert_eq!(legacy_ahead(&legacy, (Tier::Arrival, when, "r")), 1);
    }

    /// The packet says where it stands and what stands before it.
    #[test]
    fn a_waiting_request_states_its_position_and_what_is_ahead() {
        assert_eq!(
            place_text(3, 1, 1, 0, "urgent: answers urgent item 47601829"),
            "position 3: behind 1 train gate and 1 dock re-gate; urgent: answers urgent item 47601829"
        );
        assert_eq!(
            place_text(1, 0, 0, 0, "a first gate, by age"),
            "position 1: next for a free gate; a first gate, by age"
        );
        assert_eq!(
            place_text(4, 0, 0, 3, "a first gate, by age"),
            "position 4: behind 3 gates ahead in line; a first gate, by age"
        );
        assert_eq!(
            place_text(5, 2, 0, 2, "x"),
            "position 5: behind 2 train gates and 2 gates ahead in line; x"
        );
    }
}
