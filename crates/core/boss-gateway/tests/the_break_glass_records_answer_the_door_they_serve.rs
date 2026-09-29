//! THE BREAK-GLASS RECORDS ANSWER THE DOOR THEY SERVE (backlog
//! 1c4c100a, 2026-09-22).
//!
//! Both break-glass keys were enrolled on 2026-09-07, while
//! `infra/cluster/manifests/boss.yaml` set `BOSS_PUBLIC_URL` to
//! `https://playground.algedonic.dev` (b641f3ad). The cutover to
//! `https://boss.algedonic.dev` landed 2026-09-16 (793a3c33, train #398).
//! A WebAuthn credential is bound to its relying-party id and the
//! AUTHENTICATOR enforces it, so from that train on neither key could
//! open the door — and the committed record looked healthy (sign counts
//! 4 and 3, both pre-cutover), which is what kept it invisible for six
//! days until David asked.
//!
//! Nothing needed a key to notice. The records say which relying party
//! they were enrolled under (`rp_id`), the manifest says which the door
//! answers as, and both are in this tree — so the car that moves one
//! without the other is refused HERE, at its gate, instead of in an
//! emergency. Derived from the two files, never retyped (CLAUDE.md §9a).

use std::path::Path;

use boss_gateway::break_glass::{BreakGlassCredential, CREDENTIALS_MANIFEST, Door};
use webauthn_rs::prelude::Url;

/// Records KNOWN to be bound to another relying party, each awaiting
/// re-enrolment through a `break-glass-enrolment` packet — one row per
/// record, `label rp_id cutover incident`. This is the incident as the
/// tree states it, not a waiver: the pin below fails on a row whose
/// record has been replaced, so the car that commits a re-enrolled
/// record is the car that deletes its row. Any record bound elsewhere
/// and NOT listed — the next cutover — fails.
///
/// A FILE, not a const, since backlog edf403aa (2026-09-28). "The list
/// may only shrink" was a sentence in a doc comment, and a car that
/// moved `BOSS_PUBLIC_URL` could add two rows for the door it abandoned
/// and pass both checks here — the 1c4c100a incident let back in by a
/// diff only review saw. The shrink is now mechanical:
/// `infra/lint/break-glass-awaiting-re-enrolment-only-shrinks.sh` refuses
/// any row the trunk does not already carry, and it can read the rows
/// only because they are data. This test and that lint read the one
/// file (CLAUDE.md §9a).
const AWAITING_RE_ENROLMENT: &str = "infra/lint/break-glass-awaiting-re-enrolment.txt";

const DEPLOYMENT_MANIFEST: &str = "infra/cluster/manifests/boss.yaml";

/// One row of [`AWAITING_RE_ENROLMENT`]: the record it excuses and the
/// reason it may exist at all.
struct Awaiting {
    label: String,
    rp_id: String,
}

/// The rows, each held to its shape: four whitespace-separated fields,
/// and the two that justify it — the cutover commit that moved the door
/// and the backlog item that recorded the incident — spelled as the
/// short hex ids they are. A row without its reason is refused by name,
/// because a waiver nobody explained is one nobody can later judge.
fn awaiting(root: &Path) -> Vec<Awaiting> {
    let text = std::fs::read_to_string(root.join(AWAITING_RE_ENROLMENT))
        .unwrap_or_else(|e| panic!("{AWAITING_RE_ENROLMENT} is readable: {e}"));
    let hex = |s: &str, min: usize| s.len() >= min && s.bytes().all(|b| b.is_ascii_hexdigit());
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|row| {
            let fields: Vec<&str> = row.split_whitespace().collect();
            let [label, rp_id, cutover, incident] = fields[..] else {
                panic!(
                    "{AWAITING_RE_ENROLMENT} row `{row}` is not `label rp_id cutover incident` \
                     (four fields)"
                );
            };
            assert!(
                hex(cutover, 7) && hex(incident, 8),
                "{AWAITING_RE_ENROLMENT} row `{row}` must name the cutover commit (hex, 7+) \
                 and the backlog item (hex, 8+) that justify it"
            );
            Awaiting {
                label: label.to_string(),
                rp_id: rp_id.to_string(),
            }
        })
        .collect()
}

/// The door the deployment manifest serves: the host of the gateway's
/// `BOSS_PUBLIC_URL`, read from the one line that sets it.
fn door(root: &Path) -> Door {
    let text = std::fs::read_to_string(root.join(DEPLOYMENT_MANIFEST)).unwrap();
    let urls: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("name: BOSS_PUBLIC_URL,"))
        .filter_map(|l| l.split("value: \"").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .collect();
    assert_eq!(
        urls.len(),
        1,
        "{DEPLOYMENT_MANIFEST} sets BOSS_PUBLIC_URL exactly once as \
         `{{name: BOSS_PUBLIC_URL, value: \"…\"}}`; found {urls:?}"
    );
    Door::of(&Url::parse(urls[0]).unwrap()).unwrap()
}

/// Every record the credential manifest commits, parsed with the type
/// the gateway loads them with: each `data:` key `<label>.json: |` and
/// the block indented under it.
fn records(root: &Path) -> Vec<(String, BreakGlassCredential)> {
    let text = std::fs::read_to_string(root.join(CREDENTIALS_MANIFEST)).unwrap();
    let mut out = Vec::new();
    let mut lines = text
        .lines()
        .skip_while(|l| *l != "data:")
        .skip(1)
        .peekable();
    while let Some(line) = lines.next() {
        let Some(key) = line
            .strip_prefix("  ")
            .and_then(|l| l.strip_suffix(": |"))
            .filter(|k| k.ends_with(".json"))
        else {
            continue;
        };
        let mut block = String::new();
        while let Some(next) = lines.peek() {
            if !(next.starts_with("    ") || next.trim().is_empty()) {
                break;
            }
            block.push_str(next.trim_start());
            block.push('\n');
            lines.next();
        }
        let rec: BreakGlassCredential = serde_json::from_str(&block)
            .unwrap_or_else(|e| panic!("{CREDENTIALS_MANIFEST} `{key}` is not a record: {e}"));
        out.push((key.to_string(), rec));
    }
    out
}

#[test]
fn the_break_glass_records_answer_the_door_they_serve() {
    let root = boss_testing::repo_root();
    let door = door(&root);
    let records = records(&root);
    let awaiting = awaiting(&root);
    assert!(
        !records.is_empty(),
        "{CREDENTIALS_MANIFEST} commits no break-glass record — the emergency door has no key"
    );

    for (key, rec) in &records {
        assert_eq!(
            key,
            &format!("{}.json", rec.label.as_str()),
            "the ConfigMap key names the record's own label"
        );
        // No evidence is not a pass: a record that does not say which
        // party it was enrolled for cannot be held to this door.
        let Some(rp) = rec.rp_id.as_deref() else {
            panic!(
                "{CREDENTIALS_MANIFEST} `{key}` does not name the relying party it was \
                 enrolled under (`rp_id`), so nothing can say whether it opens {}",
                door.rp_id
            );
        };
        let listed = awaiting
            .iter()
            .any(|a| a.label == rec.label.as_str() && a.rp_id == rp);
        assert!(
            rp == door.rp_id || listed,
            "{CREDENTIALS_MANIFEST} `{key}` was enrolled for `{rp}`, and {DEPLOYMENT_MANIFEST} \
             serves the door as `{}` — the key cannot open it. A change to BOSS_PUBLIC_URL \
             re-enrols every break-glass key (a `break-glass-enrolment` packet per key) in \
             the same breath; this is backlog 1c4c100a",
            door.rp_id
        );
    }

    for Awaiting {
        label,
        rp_id: bound,
    } in &awaiting
    {
        assert_ne!(
            *bound, door.rp_id,
            "`{label}` is listed in {AWAITING_RE_ENROLMENT} as awaiting re-enrolment from \
             `{bound}`, which is this door"
        );
        assert!(
            records
                .iter()
                .any(|(_, r)| r.label.as_str() == label && r.rp_id.as_deref() == Some(bound)),
            "`{label}` is no longer bound to `{bound}` — it was re-enrolled or removed, so \
             delete its row from {AWAITING_RE_ENROLMENT}: the list only shrinks"
        );
    }
}
