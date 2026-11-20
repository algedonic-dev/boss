//! The reads a green does not make ride the review steps themselves
//! (backlog cfa2ce64, 2026-11-17). On 2026-11-16 five reviews each found
//! a defect the car's gate had passed: an exit path after a mutating
//! verb's first write that left state no door restored (cf45a112,
//! 1c413d8d), an unbounded write onto a packet and two cars of one
//! lineage that conflicted with each other but not with main (75bb4029),
//! a false present-tense sentence in public prose (04f504da, 75ea1f87,
//! 21da8abe), and a curl stub that did not answer as curl does (gate
//! 539ccf80). The drain's briefs then said these by hand. They live in
//! the `review` and `landed-review` procedures — registry data the
//! reviewer's brief prints as the step's own specification — rather
//! than in a rules document, and this pin names which read was dropped
//! if a later version loses one.

use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

fn procedure(wf: &WorkflowSpec, title: &str) -> String {
    wf.steps
        .iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("{} has no `{title}` step", wf.kind))
        .metadata_defaults
        .get("procedure")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Each read, by the phrase a reviewer cannot satisfy without making it.
const EVERY_REVIEW_READS: [(&str, &str); 4] = [
    (
        "kill -9",
        "walk every exit of a mutating verb after its first write",
    ),
    (
        "packet_budget::BUDGET",
        "bound a write onto a packet under the event budget",
    ),
    (
        "present-tense sentence",
        "check public prose against the code at the head",
    ),
    (
        "every flag the code passes",
        "hold a test stub to the tool it stands in for",
    ),
];

#[test]
fn the_held_review_makes_all_five_reads_including_the_dock_conflict() {
    let p = procedure(&bundled("ship-a-change"), "review");
    for (phrase, read) in EVERY_REVIEW_READS {
        assert!(p.contains(phrase), "`review` must {read} (`{phrase}`): {p}");
    }
    assert!(
        p.contains("git merge-tree --write-tree"),
        "`review` must merge the car against every open car touching its files, \
         not only origin/main (review 75bb4029): {p}"
    );
    // Review f146734f B1 (2026-11-17): the dock queue's predicate leaves
    // every HELD car out, so a reviewer told it lists the open cars never
    // merged against the held peer 909a8593 that shared this car's files.
    assert!(
        p.contains("GET /api/jobs?kind=ship-a-change&status=open"),
        "`review` must list the open cars from a read that includes held ones: {p}"
    );
    assert!(
        !p.contains("loading-dock/queue lists them"),
        "the dock queue omits held cars, so it is not the list of open cars: {p}"
    );
    // Review f146734f N1 and B2: two lineage blocks numbered alike merged
    // with exit 0, so the exit code alone is not the read.
    assert!(
        p.contains("read the merged content"),
        "`review` must read what merge-tree merged, not only its exit code: {p}"
    );
}

#[test]
fn the_landed_review_makes_the_four_that_still_apply_after_landing() {
    let p = procedure(&bundled("ship-a-change"), "landed-review");
    for (phrase, read) in EVERY_REVIEW_READS {
        assert!(
            p.contains(phrase),
            "`landed-review` must {read} (`{phrase}`): {p}"
        );
    }
}

/// Run 1e888004 (2026-11-16) reviewed 86 newly public files by carrying
/// 84 lines from an earlier review whose files had not changed since its
/// measured sha, and reading the 2 that had. That is the procedure now,
/// so the next analyst neither re-reads 84 files nor carries a changed one.
#[test]
fn the_publish_review_carries_only_what_has_not_changed_since_it_was_read() {
    let p = procedure(&bundled("publish-to-github"), "review");
    for phrase in [
        "CARRY WHAT AN EARLIER REVIEW READ",
        "git diff --quiet <that source_sha> <source_sha> -- <path>",
        "unchanged since <that source_sha>",
        "read in full",
    ] {
        assert!(p.contains(phrase), "`review` must say `{phrase}`: {p}");
    }
}
