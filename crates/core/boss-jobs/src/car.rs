//! Shared builders for a ship-a-change "car": the packet body and the
//! per-step evidence, plus the `Receipt` type they carry.
//!
//! WHY THIS IS IN CORE. Filing a car — POST the ship-a-change packet,
//! then fill its scope/build/gate steps with the receipt copied
//! verbatim — is done two ways now: by `boss park` (a human parks after
//! a green gate) and by the dispatcher's auto-park handler (a rule parks
//! on the gate-run's green step). If each built the packet its own way
//! they would drift, and the one field that must never be rebuilt is the
//! receipt (that is the bug `boss park` was created to kill). So the
//! pure builders live here, shared by both callers; the HTTP
//! orchestration (the POST and the PUTs) stays with each caller, because
//! that part legitimately differs. CLAUDE.md 9a: collapse the fact that
//! would otherwise live twice.

use serde_json::{Value, json};

pub fn select_receipt(job: &serde_json::Value) -> Option<serde_json::Value> {
    job.pointer("/metadata/regate_receipt")
        .filter(|v| !v.is_null())
        .and_then(parse_receipt)
        .or_else(|| {
            // Field order is not a contract, so parse the gate step's
            // receipt rather than scanning for it.
            job.get("steps")
                .and_then(|s| s.as_array())
                .into_iter()
                .flatten()
                .find_map(|step| step.pointer("/metadata/receipt").and_then(parse_receipt))
        })
}

/// A receipt is stored two ways: as a JSON STRING (the gate step, and a
/// re-rail that copied that string verbatim into `regate_receipt`) or as
/// an OBJECT (tooling that parsed before writing). Both are receipts —
/// the same pair the boarding logic accepts (train.rs). Returns the
/// receipt object, or None if the value is neither an object nor a string
/// that parses to one.
fn parse_receipt(v: &serde_json::Value) -> Option<serde_json::Value> {
    match v {
        serde_json::Value::String(s) => serde_json::from_str(s).ok(),
        obj @ serde_json::Value::Object(_) => Some(obj.clone()),
        _ => None,
    }
}

/// What a green gate-run packet says about a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// The receipt string, copied verbatim — never rebuilt from parts.
    pub raw: String,
    /// The head it vouches for, read back out for refusal messages and
    /// for the caller to compare against the branch.
    pub head: String,
    /// `full`, `--auto`, or whatever the runner was given.
    pub mode: String,
}

/// PURE: why a green receipt must NOT become a car or refresh one —
/// `None` for an ordinary receipt.
///
/// THE BACKSTOP (backlog 3e63662c; the adversarial reviews of c9439bd1
/// and cc7273c0). `infra/gate.sh` writes `estate` on a receipt only for
/// a run that declared it had no estate (infra/lint/lib/no-estate.sh),
/// so that run's live comparisons were never made. Only the public
/// mirror may declare it, the lib refuses the declaration anywhere but
/// a GitHub-hosted runner, and the mirror parks nothing — so a receipt
/// carrying `estate` that reaches a car is a leak past both. The gate's
/// other readers (the train gate, the yard, the flake tally) judge the
/// verdict alone; this is asked by the three that turn a green into a
/// car or refresh one: the auto-park handler, `boss park` and `boss
/// rerail --finish` (the last two through `park::receipt_for`).
///
/// THE SECOND DECLARATION (backlog 6665aa97; review 30397e74 of car
/// 4f1a96ab). `BOSS_TRUNK=none` (infra/lint/lib/trunk-ref.sh) is the
/// mirror's too, behind the same runtime guard, and gate.sh writes
/// `trunk` on its receipt naming the lints that judged no car against a
/// trunk. Until this, a receipt carrying `trunk` without `estate` parked.
/// Every mirror receipt carries both today, so that is a leak past the
/// guard and is refused the same way. The function keeps its first name
/// because its three callers already ask it by that name.
///
/// FAILS CLOSED: a receipt that does not parse as a JSON object cannot
/// say whether it declared, and an unread declaration is not an absent
/// one, so it is refused too.
pub fn declared_no_estate(raw: &str) -> Option<String> {
    let Ok(receipt @ Value::Object(_)) = serde_json::from_str::<Value>(raw) else {
        return Some(format!(
            "refusing: the receipt does not parse as a JSON object, so whether its run \
             declared no estate or no trunk cannot be read — an unread declaration is not \
             an absent one (infra/lint/lib/no-estate.sh, infra/lint/lib/trunk-ref.sh). \
             receipt: {raw}"
        ));
    };
    if let Some(estate) = receipt.get("estate") {
        return Some(format!(
            "refusing: this green's receipt declares no estate ({estate}), so the live \
             comparisons it names were never made. Only the public mirror may declare that, \
             and it parks nothing; a cluster gate carrying it is a leaked declaration \
             (infra/lint/lib/no-estate.sh). Find what set it, remove it, and re-gate."
        ));
    }
    let trunk = receipt.get("trunk")?;
    Some(format!(
        "refusing: this green's receipt declares no per-car trunk ({trunk}), so the trunk \
         comparisons of the lints it names were never made. Only the public mirror \
         may declare that, and it parks nothing; a cluster gate carrying it is a leaked \
         declaration (infra/lint/lib/trunk-ref.sh). Find what set it, remove it, and re-gate."
    ))
}

/// The instant stamp a car's gate step and the conductor's `review`
/// step share. RFC3339 to the SECOND, `Z`: dock queue time is
/// `review.completed_at` minus `gate.completed_at`, so sub-second
/// precision or an offset would break that subtraction by writer. ONE
/// definition — `boss gate`'s `stamp` delegates here — so the two ends
/// of the measurement cannot drift in format.
pub fn stamp(now: chrono::DateTime<chrono::Utc>) -> String {
    now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The ship-a-change packet body for a car. `owner` is who answers
/// for it — the platform owner as `boss_core::platform_owner` resolved
/// it, or `NOBODY` when it refused (backlog 3c23662d: this line named
/// one person, on every car on every deployment, until 2026-09-18).
pub fn car_body(
    branch: &str,
    summary: &str,
    backlog_item: Option<&str>,
    delivery_channel: Option<&str>,
    owner: &str,
) -> Value {
    let mut metadata = json!({ "branch": branch, "summary": summary });
    if let Some(item) = backlog_item {
        // A declared job edge — ref-checked by the API at the write,
        // which is what makes it safe to write here rather than by hand.
        // A mistyped id is refused instead of silently pointing at
        // nothing.
        metadata[BACKLOG_ITEM] = json!(item);
    }
    // How this change ships (data/config/software/infra), derived at the
    // gate and carried here so the yard and channel-gated delivery read
    // it off the car. Absent when the gate could not classify the diff.
    if let Some(dc) = delivery_channel {
        metadata["delivery_channel"] = json!(dc);
    }
    json!({
        "kind": "ship-a-change",
        "title": summary_title(summary),
        "subject": {"subject_kind": "custom", "id": branch},
        "owner_id": owner,
        "priority": "standard",
        "status": "open",
        "tags": [],
        "metadata": metadata,
    })
}

/// A car's title: the first sentence of its summary, trimmed.
///
/// Titles are what David reads on a board, so they get the summary's
/// opening claim rather than the branch name — `fix/a-dropped-lookup`
/// says less than "A dropped lookup does not red a train".
fn summary_title(summary: &str) -> String {
    let first = summary
        .split_terminator(['.', '\n'])
        .next()
        .unwrap_or(summary)
        .trim();
    let t = if first.is_empty() {
        summary.trim()
    } else {
        first
    };
    t.chars().take(120).collect()
}

/// The step titles a parked car fills, in the order the workflow runs
/// them. `ship-a-change` names them in its registry row, so a rename
/// there is a rename here — a caller refuses rather than guesses when a
/// step is missing.
pub const SCOPE: &str = "Declare the boundary";
pub const BUILD: &str = "Build it";
pub const GATE: &str = "Green, and observed working";

/// The trigger step a car OPENS at. `ship-a-change`'s first step, with
/// `ready_when: true`, so the POST that files the packet completes it
/// and `scope` is ready the moment the car exists.
pub const OPENED: &str = "Change started";

/// The registry SLUGS for the same steps — the primary key `find_step`
/// looks a step up by, with the titles above as its fallback. Paired
/// with their titles in one place so a lookup cannot drift from the
/// evidence written through it (CLAUDE.md §9a); `step_fields` carries
/// the pair through to every caller rather than letting each re-derive
/// it.
pub const OPENED_SLUG: &str = "opened";
pub const SCOPE_SLUG: &str = "scope";
pub const BUILD_SLUG: &str = "build";
pub const GATE_SLUG: &str = "gate";
pub const REVIEW_SLUG: &str = "review";

/// The evidence each of the three steps carries, and when it was filled,
/// as `(registry slug, step title, metadata)`.
///
/// When a car is filed at GREEN all three stamps are the same instant,
/// because filing it is one act. When a builder OPENED the car at build
/// start they are not: `scope` was filled then, and only the steps still
/// open are written now — see [`finish_writes`], which is what callers
/// use. What the stamps separate is the *dock* — gate's stamp is when
/// the car became ready, and the conductor's stamp on `review` is when
/// it boarded, so the difference is queue time and nothing else.
pub fn step_fields(
    summary: &str,
    excludes: &str,
    test: &str,
    verified: &str,
    receipt: &Receipt,
    now: chrono::DateTime<chrono::Utc>,
) -> [(&'static str, &'static str, Value); 3] {
    let at = stamp(now);
    [
        (
            SCOPE_SLUG,
            SCOPE,
            json!({"summary": summary, "excludes": excludes, "completed_at": at}),
        ),
        (BUILD_SLUG, BUILD, json!({"test": test, "completed_at": at})),
        (
            GATE_SLUG,
            GATE,
            json!({
                "gates": if receipt.mode.is_empty() { "full" } else { &receipt.mode },
                // VERBATIM. The whole point of the shared builder.
                "receipt": receipt.raw,
                "verified": verified,
                "completed_at": at,
            }),
        ),
    ]
}

/// The review step a parked car waits at. The conductor reads it as
/// the dock: a car whose review is `ready`/`active` is parked, and
/// boarding stamps `metadata.train` rather than completing it (so a
/// cancelled train can release the car by clearing the stamp).
pub const REVIEW: &str = "Open for review";

/// The review a car no rule held owes AFTER it lands (backlog bd331d38,
/// option A): ship-a-change's `landed-review` step, its title, and the
/// station of the same name that counts every open car whose step is
/// ready or active. One spelling for the readers that surface and
/// dispatch that debt — `boss orient`, MY WORK, `boss dispatch`, `boss
/// step complete` (backlog c851ed24) — pinned to the protocol row and
/// the station file by `a_car_no_rule_held_owes_its_review_after_landing`.
pub const LANDED_REVIEW_SLUG: &str = "landed-review";
pub const LANDED_REVIEW: &str = "Reviewed after landing";
pub const LANDED_REVIEW_STATION: &str = "landed-review";

/// The review-step key a REVIEW hold records its judged car head under,
/// beside `hold` (design 7cedfa29 D2, backlog b7b02024). Written by the
/// doors that hold a car for its adversarial review — the gate's
/// `--hold` through the auto-park handler, `boss rerail --finish`, and
/// the conductor's re-judge — and read by the conductor at boarding: a
/// car carrying it boards only on a `release` recorded at its current
/// head. One spelling, because two crates write it and one reads it.
pub const HOLD_SHA: &str = "hold_sha";

/// The GATE-RUN key that says its `hold` is not a review's: the gate's
/// judge could not READ the diff (a git blip), and no `--hold` was given.
/// The auto-park handler then writes the hold WITHOUT [`HOLD_SHA`], so
/// clearing it sends the car back to the dock's judge rather than to a
/// reviewer (backlog b7b02024, review F1).
pub const HOLD_UNBOUND: &str = "hold_unbound";

/// The review-step key that reserves a held car's release for one named
/// actor (design 393bdef6 Q1, David 2026-10-16; backlog 0cfc57e6). The
/// release rule lifts a review hold on its reviewer's RELEASE verdict,
/// for every hold class; a car carrying this key waits for a person, as
/// every car did before the rule. Written by `boss hold --releaser`,
/// read by [`crate::car_release::judge`], which refuses an automation
/// caller while it names anyone. A person's release is never refused by
/// it. Data on the car, so the override needs no deploy and leaves a
/// record of who reserved what.
pub const RELEASER: &str = "releaser";

/// The review-step key the release rule records why it LEFT a held car
/// under: `{review, outcome, why, by, at}`, the release door's refusal
/// as the door said it (backlog 0cfc57e6). A rule that declines and says
/// nothing is a silent failure; `boss-api GET` and the yard read the
/// review step, so the next reader finds the reason beside the hold.
pub const AUTO_RELEASE: &str = "auto_release";

/// The review-step key the release record lives under: `{sha, review,
/// verdict, by, at, main_sha}` (design 7cedfa29 D3), written by `boss
/// release --review`.
pub const RELEASE: &str = "release";

/// The merge-door body that puts a hold NOT bound to a head on a car's
/// review step, whose metadata is `review_md` as it stands: an
/// operator's `boss hold`, the conductor's red-train hold, an unread
/// diff, a retirement putting its brake back. A review hold adds
/// [`HOLD_SHA`] to this body itself.
///
/// It also deletes a [`HOLD_SHA`] that a release has already lifted
/// (backlog 6c463c66, from review 4822764d). Once a car is released, its
/// review step keeps `release`, `hold_sha` and `agent_run`, and no
/// writer removes them. Before this, a hold written afterwards wrote
/// `hold` alone and left the old `hold_sha` beside it, so every reader
/// that reads `hold_sha` as "this hold was judged at that head" was
/// fooled by it. The release door judged the yard's head-less request
/// at that head, and a release CARRIED over the dock's replay (`release.
/// sha` H2, the old `hold_sha` still H1) then read releasable and was
/// lifted again on its old review. `boss release` told the operator
/// that a red-train hold was a review hold that the conductor would
/// re-hold, when the release already boards the car.
///
/// It deletes the key only when the step records a `release` and no
/// hold is on: the review hold that `hold_sha` names was lifted, and
/// the new one was judged at no head. A live review hold keeps its
/// head, so a hand hold over it is still a review hold. A review hold
/// cleared bare, with no release, keeps it too, because the conductor
/// re-holds such a car for its review. Deleting it costs the dock
/// nothing: it reads a `release` before it reads `hold_sha`, and no
/// writer removes a `release`.
pub fn hold_body(review_md: &Value, reason: &str) -> Value {
    let mut body = json!({ "hold": reason.trim() });
    let lifted = review_md.get(RELEASE).is_some_and(Value::is_object)
        && crate::stranded::marked(review_md, "hold").is_none()
        && review_md.get(HOLD_SHA).is_some_and(|h| !h.is_null());
    if lifted {
        body[HOLD_SHA] = Value::Null;
    }
    body
}

/// PURE: the merge-door body that carries a car's live REVIEW hold to
/// `head`, the head a refresh just gave the car, or `None` when there is
/// nothing to carry. A REFRESH THAT MOVES THE CAR'S HEAD MOVES ITS HOLD'S
/// HEAD (backlog 6b12a70f).
///
/// WHY. A review hold says "waits for a review of this head", and
/// [`HOLD_SHA`] is that head: the release door judges a head-less ask
/// (the yard, the release rule) at it, and the conductor re-holds at the
/// head it boards. Both refresh doors — `boss rerail --finish` and the
/// auto-park handler's `ParkAction::Refresh` — wrote a hold only when
/// the NEW green brought a reason of its own, so a car already held kept
/// the old head whenever the refreshed diff was clear. Measured on car
/// eb86ac0c, 2026-11-05: held by the conductor's re-judge at 29943365,
/// refreshed in place onto the green at df139b70, the review of df139b70
/// (run 7cb2438c, RELEASE) was refused by the release rule and by `boss
/// release --review` — "run 7cb2438c reviewed df139b70, and the car's
/// head is 29943365" — at a head the forge no longer had, and no
/// supported door moved it. The conductor's re-judge would have moved it
/// (`review_hold_write` in boss-cli, afc5147c), but it judges only cars
/// that are not held, so it never saw this one.
///
/// ONE DECIDER, BOTH DOORS: they ask this, so the two refreshes cannot
/// disagree about it. Only the head is written. The hold's reason stands,
/// because the car still waits for its review, and rewriting the reason
/// would put back a hold lifted between the reader's read and this write.
/// A hold bound to no head (a hand hold, a red-train hold, an unread
/// diff) binds none here: it is not a review's, and binding it would ask
/// a reviewer for what a person must answer.
pub fn hold_follows_head(review_md: &Value, head: &str) -> Option<Value> {
    let head = head.trim();
    crate::stranded::marked(review_md, "hold")?;
    let held_at = review_md
        .get(HOLD_SHA)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    (!head.is_empty() && held_at != head).then(|| json!({ HOLD_SHA: head }))
}

/// PURE: what a write [`hold_follows_head`] decided was judged on — the
/// review step's `hold` and [`HOLD_SHA`], raw, as `review_md` read them
/// (`null` = absent). Both refresh doors send it as the merge door's
/// precondition ([`crate::step_metadata_write::EXPECT_HEADER`], backlog
/// cd1f68c6), so the head moves only while the step still carries the
/// hold that was read. Each door wrote `{hold_sha: head}` many calls
/// after its read with no check, so a release landing in that window
/// left a `hold_sha` beside no hold, and a person's hold or a red-train
/// hold that replaced the review hold became bound to the new head — a
/// hold a reviewer's release could then lift, where only a person
/// should.
pub fn hold_as_judged(review_md: &Value) -> serde_json::Map<String, Value> {
    ["hold", HOLD_SHA]
        .into_iter()
        .map(|key| {
            (
                key.to_string(),
                review_md.get(key).cloned().unwrap_or(Value::Null),
            )
        })
        .collect()
}

/// The words the conductor's judged-red hold opens with, before and
/// after the train it names by its first eight characters.
const JUDGED_RED_OPENS: &str = "held by the conductor: train ";
const JUDGED_RED_SAYS: &str = " was judged red";

/// The head of the hold the conductor writes on a car a judged red names
/// (`judged_red_hold_reason` in boss-cli): "held by the conductor: train
/// <train_id8> was judged red", the rest of the reason after it. ONE
/// SPELLING, read back by [`judged_red_hold_train`], so the writer and
/// the reader cannot drift (CLAUDE.md §9a).
pub fn judged_red_hold_head(train_id8: &str) -> String {
    format!("{JUDGED_RED_OPENS}{}{JUDGED_RED_SAYS}", train_id8.trim())
}

/// PURE: the train (its first eight characters, as the hold spells it)
/// a hold's text says was judged red, when the text is the conductor's
/// judged-red hold ([`judged_red_hold_head`]); `None` for any other hold.
pub fn judged_red_hold_train(hold: &str) -> Option<&str> {
    let rest = hold.trim().strip_prefix(JUDGED_RED_OPENS)?;
    let (train, _) = rest.split_once(JUDGED_RED_SAYS)?;
    (!train.is_empty() && !train.contains(char::is_whitespace)).then_some(train)
}

/// PURE: the merge-door body that lifts a car's red-train hold when a
/// strike release (`boss release --diagnosis-file`) has cleared the
/// strike that hold stands for, or `None` when the hold is not that
/// release's to lift. A STRIKE RELEASE LIFTS THE HOLD ITS STRIKE PLACED
/// (backlog 79129752, the strike-release sibling of 6b12a70f).
///
/// WHY. A judged red does two things to the car it names: the conductor
/// strikes it (`red_trains`) and holds its review step with
/// [`judged_red_hold_head`]'s text, bound to no head. The strike release
/// cleared the first and left the second. Measured on car 775feba5,
/// 2026-11-05: train 9b5c1889's gate 3082de78 went red at 06:17Z with
/// the car the only one aboard; at 07:47Z the look named that gate-run,
/// cleared `red_trains` and appended `strike_releases`, the car re-gated
/// green, and the review step still read "held by the conductor: train
/// 9b5c1889 was judged red ...", so the car never reached the dock.
///
/// It lifts ONLY when every one of these holds:
///
/// - no strike remains (`remaining_strikes`, read back after the look);
/// - the live hold is the judged-red hold, for a train in
///   `cleared_trains` (the full ids of the trains whose gate-runs the
///   look verified, matched on the eight characters the hold names);
/// - the hold is bound to no head ([`HOLD_SHA`]): a head-bound hold is a
///   review's, and only a review lifts it;
/// - the release is reserved for no one ([`RELEASER`]): a reserved car
///   waits for that person, whatever its hold says.
///
/// A hand hold, a review hold, another train's red, or a strike still
/// standing keeps its hold. Only `hold` is written; the caller records
/// what it lifted beside it, as a bare release does.
pub fn hold_lifted_by_strike_release(
    review_md: &Value,
    cleared_trains: &[String],
    remaining_strikes: i64,
) -> Option<Value> {
    if remaining_strikes > 0 || review_md.get(RELEASER).is_some() {
        return None;
    }
    let head_bound = review_md
        .get(HOLD_SHA)
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty());
    if head_bound {
        return None;
    }
    let hold = crate::stranded::marked(review_md, "hold")?;
    let named = judged_red_hold_train(&hold)?;
    cleared_trains
        .iter()
        .any(|t| t.trim().chars().take(8).collect::<String>() == named)
        .then(|| json!({ "hold": Value::Null }))
}

/// One step completion a car's filer owes: which step, the evidence to
/// MERGE onto it, and the status-only body that then completes it.
///
/// TWO WRITES, IN THIS ORDER (backlog e39a9d2a, car 2 of its plan). This
/// was one PUT of `{status, metadata}` built fresh, with no read. The
/// step PUT REPLACES metadata wholesale, and the registry materializes
/// keys onto every step at admission (`metadata_defaults`,
/// `authority_role`, `station`, `audience`, `claimable`), so every
/// `boss car open`, `boss park` and auto-park shed them. Now the
/// evidence goes through the step merge door — [`StepWrite::merge_path`],
/// one transaction against the row as it stands, so it cannot race — and
/// then [`StepWrite::status_path`] takes a body carrying nothing to
/// drop. Merge FIRST: a step's required-at-done fields are validated
/// when it flips to completed, so the evidence must already be there.
///
/// The step ID is read OFF the car, never assembled — the same rule the
/// receipt lives by, for the same reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepWrite {
    /// The step's id, as the packet reported it.
    pub step_id: String,
    /// Its title, so a message can name what was written.
    pub title: &'static str,
    /// `PATCH /api/jobs/{car}/steps/{step_id}/metadata` body: top-level
    /// keys merged into what the step already holds.
    pub metadata: Value,
    /// `PUT /api/jobs/{car}/steps/{step_id}` body, sent AFTER the merge:
    /// the status alone.
    pub status_body: Value,
}

impl StepWrite {
    fn completing(step_id: &str, title: &'static str, metadata: Value) -> Self {
        Self {
            step_id: step_id.to_string(),
            title,
            metadata,
            status_body: json!({"status": "completed"}),
        }
    }

    /// The step merge door on `job_id` — the FIRST write. The job is the
    /// car for its own steps, and the linked item for
    /// [`triage_on_park`]'s.
    pub fn merge_path(&self, job_id: &str) -> String {
        format!("/api/jobs/{job_id}/steps/{}/metadata", self.step_id)
    }

    /// The step PUT on `job_id` — the SECOND write, carrying
    /// [`StepWrite::status_body`].
    pub fn status_path(&self, job_id: &str) -> String {
        format!("/api/jobs/{job_id}/steps/{}", self.step_id)
    }
}

/// PURE: the id of `(slug, title)` on this car, or `None` when the
/// packet has no such step. The id a CLAIM needs
/// (`POST …/steps/{id}/claim`), which takes no body and so has nowhere
/// to carry a [`StepWrite`].
pub fn step_id_for(car: &Value, slug: &str, title: &str) -> Option<String> {
    find_step(car, slug, title)?
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Has this step already been taken through? A completed or skipped step
/// is frozen — the step API refuses a metadata write to one and its 409
/// names the job-metadata door instead — so every writer below skips it
/// rather than discovering that at the wire.
fn already_done(step: &Value) -> bool {
    matches!(
        step.get("status").and_then(Value::as_str),
        Some("completed" | "skipped")
    )
}

/// The missing-step refusal both writers share: an in-flight packet is
/// pinned to the workflow version it was admitted under, so a car whose
/// steps do not answer to the slugs this code knows must refuse rather
/// than write evidence onto whatever step happened to be there.
fn no_such_step(slug: &str, title: &str) -> String {
    format!(
        "this car has no `{slug}` step ('{title}') — refusing to write evidence onto a \
         packet whose shape does not match the ship-a-change steps this build of boss \
         knows. An in-flight car is pinned to the workflow version it was admitted \
         under; read the packet before writing to it."
    )
}

/// PURE: the writes that OPEN a car at build start — `scope` completed
/// with the brief the builder was handed.
///
/// WHY THE SCOPE IS WRITTEN AT OPEN AND NOT AT GREEN. `scope` asks what
/// the change contains and what it deliberately excludes, and the
/// registry row says why: *"it asks a person ... BEFORE the work, which
/// is the only moment that decision keeps a PR small."* Filed at green
/// it was being recorded after the fact. A builder opening the car has
/// exactly that brief in hand, so it is stated when it is true.
///
/// `build` is NOT written here. It is taken through the CLAIM door
/// (`POST …/steps/{id}/claim`), a Ready→Active compare-and-set that
/// records the claimant and refuses a second one with a 409 naming the
/// holder — which is how "two agents on the same branch" becomes visible
/// instead of becoming a twin car.
///
/// IDEMPOTENT: a car whose scope is already declared yields no writes,
/// so a builder re-running the verb writes nothing rather than 409ing on
/// a frozen step.
pub fn open_writes(
    car: &Value,
    summary: &str,
    excludes: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<StepWrite>, String> {
    let Some(step) = find_step(car, SCOPE_SLUG, SCOPE) else {
        return Err(no_such_step(SCOPE_SLUG, SCOPE));
    };
    if already_done(step) {
        return Ok(Vec::new());
    }
    let Some(step_id) = step.get("id").and_then(Value::as_str) else {
        return Err(format!("the `{SCOPE_SLUG}` step on this car has no id"));
    };
    let at = stamp(now);
    Ok(vec![StepWrite::completing(
        step_id,
        SCOPE,
        json!({"summary": summary, "excludes": excludes, "completed_at": at}),
    )])
}

/// PURE: the writes a GREEN owes a car — each of scope/build/gate that
/// is not already completed, carrying [`step_fields`]' evidence.
///
/// ONE DEFINITION OF "WHAT A GREEN OWES A CAR", for the three callers
/// that owe it: `boss park`, the dispatcher's auto-park handler, and now
/// both of those acting on a car a builder OPENED rather than one they
/// just filed. The two cases differ only in which steps are already
/// done, which is a fact ON THE PACKET — so it is read off the packet
/// here instead of each caller deciding (CLAUDE.md §9a).
///
/// THE SKIP IS THE POINT. A car opened at build start has `scope`
/// completed already, and the step API refuses a metadata write to a
/// completed step. A green that re-sent `scope` would 409 on every car a
/// builder opened — which is to say, on every car, once they do.
pub fn finish_writes(
    car: &Value,
    summary: &str,
    excludes: &str,
    test: &str,
    verified: &str,
    receipt: &Receipt,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<StepWrite>, String> {
    let mut out = Vec::new();
    for (slug, title, metadata) in step_fields(summary, excludes, test, verified, receipt, now) {
        let Some(step) = find_step(car, slug, title) else {
            return Err(no_such_step(slug, title));
        };
        if already_done(step) {
            continue;
        }
        let Some(step_id) = step.get("id").and_then(Value::as_str) else {
            return Err(format!("the `{slug}` step on this car has no id"));
        };
        out.push(StepWrite::completing(step_id, title, metadata));
    }
    Ok(out)
}

/// The car-metadata key naming the actor that built it: the one the
/// claim door reads to keep that actor off the car's own review
/// ([`crate::station_claim::reviews_its_own_build`]).
pub const BUILT_BY: &str = "built_by";

/// The metadata a car carries about its own BUILD, stamped when the
/// builder opens it.
///
/// THIS IS THE FACT THE MIDDLE THIRD RENDERS (backlog be025b44, design
/// 9e82ee62). Four builders ran for 19–47 minutes each on 2026-09-09 and
/// the yard showed an empty dock throughout, because a car's packet did
/// not exist until its gate went green. Who is building, on which host,
/// in which worktree, since when — none of it was anywhere. It is four
/// keys, and a board is a reader of them.
///
/// ABSENT, NEVER NULLED. The job-metadata door DELETES a key set to
/// null, so an unknown host must omit its key rather than strip one an
/// earlier write recorded.
pub fn build_start(
    actor: &str,
    host: &str,
    worktree: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("build_started_at".to_string(), json!(stamp(now)));
    let mut put = |k: &str, v: &str| {
        let v = v.trim();
        if !v.is_empty() {
            m.insert(k.to_string(), json!(v));
        }
    };
    put(BUILT_BY, actor);
    put("build_host", host);
    put("build_worktree", worktree);
    m
}

/// The car-metadata key that says which verb ADMITTED a car at build
/// start, rather than a builder's `boss car open` or a green.
///
/// WHY (backlog 68a8726e, design 689d1f7f car 1). Measured 2026-11-08
/// ~21:5xZ: 299 of the 300 newest ship-a-change packets were opened by
/// the auto-park rule at GREEN, so no open car stood at `build` or
/// `gate` while four builders and four gates ran, and the engineering
/// station's ship-a-change line could not show work it had. `boss
/// dispatch --started` now admits the car when the build starts; this
/// marker names that door on the car, so a reader can tell an admitted
/// car from one a builder opened by hand.
pub const ADMITTED_BY: &str = "admitted_by";

/// The gate-step key naming the gate-run that HOLDS the car's `gate`
/// step: written when `boss gate` launches on a car still at build,
/// read when that gate-run's verdict is not green (see
/// [`gate_return`]). One spelling for the writer and the reader.
pub const GATE_RUN: &str = "gate_run";

/// The gate-step key a non-green verdict leaves behind when it hands
/// the step back: `{gate_run, verdict}` — the receipt stays where it was
/// earned, on the gate-run, and the car names it.
pub const LAST_VERDICT: &str = "last_verdict";

/// The writes a gate LAUNCH owes a car still at build: the `gate` step
/// stamped with the gate-run holding it, judged on no ending having
/// claimed the car — the one fence it shares with [`ending_claim`] —
/// then `build` completed (its `test` is required at done), then — when
/// the gate step is not already held — claimed, which makes it ACTIVE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateLaunch {
    /// `build` completed, or `None` when it already was.
    pub build: Option<StepWrite>,
    /// The `gate` step's id.
    pub gate_step: String,
    /// Merged onto the `gate` step FIRST — before `build` is handed in
    /// and before the claim — judged on [`GateLaunch::fence`], so the
    /// step is never active without naming the gate-run that holds it,
    /// and a launch never moves a car an ending has claimed.
    pub gate_merge: Value,
    /// What the gate step must still read when [`GateLaunch::gate_merge`]
    /// lands: no ending claimed the car ([`launch_fence`]). Refused, the
    /// launch writes nothing more (backlog 366c9fb1, Codex R4b).
    pub fence: serde_json::Map<String, Value>,
    /// Claim the `gate` step through the claim door. `false` when it is
    /// already active — a second launch on a car already in the gate
    /// only restamps which gate-run holds it.
    pub claim: bool,
}

/// PURE: what a gate launch writes on `car` — or `None` when the car is
/// past its gate (its `gate` step completed or skipped), so a launch has
/// nothing to move.
///
/// WHY THE BUILD COMPLETES AT LAUNCH (backlog 68a8726e). `gate` is
/// `ready_when = steps.build.done`, and a completed step never moves
/// backwards — so the only way a car can stand AT its gate while the
/// gate runs is that the build was handed in when the branch was handed
/// to the gate, which is exactly what a launch is. A red therefore does
/// not reopen `build`; it hands `gate` back to READY ([`gate_return`]),
/// where the next launch claims it again.
pub fn gate_launch(
    car: &Value,
    gate_run: &str,
    head: &str,
    test: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<GateLaunch>, String> {
    let Some(gate) = find_step(car, GATE_SLUG, GATE) else {
        return Err(no_such_step(GATE_SLUG, GATE));
    };
    if already_done(gate) {
        return Ok(None);
    }
    let Some(gate_step) = gate.get("id").and_then(Value::as_str) else {
        return Err(format!("the `{GATE_SLUG}` step on this car has no id"));
    };
    let Some(build) = find_step(car, BUILD_SLUG, BUILD) else {
        return Err(no_such_step(BUILD_SLUG, BUILD));
    };
    let at = stamp(now);
    let build = if already_done(build) {
        None
    } else {
        let Some(id) = build.get("id").and_then(Value::as_str) else {
            return Err(format!("the `{BUILD_SLUG}` step on this car has no id"));
        };
        Some(StepWrite::completing(
            id,
            BUILD,
            json!({"test": test, "completed_at": at, GATE_RUN: gate_run}),
        ))
    };
    Ok(Some(GateLaunch {
        build,
        gate_step: gate_step.to_string(),
        gate_merge: json!({GATE_RUN: gate_run, "gated_head": head, "launched_at": at}),
        fence: launch_fence(),
        claim: gate.get("status").and_then(Value::as_str) != Some("active"),
    }))
}

/// The open car whose `gate` step is ACTIVE and held by `gate_run` —
/// the one car a verdict of that gate-run moves. A car whose gate step
/// another launch has since restamped is not this gate's to move.
pub fn car_in_gate_run<'a>(cars: &'a [Value], gate_run: &str) -> Option<&'a Value> {
    cars.iter().find(|c| {
        is_open(c)
            && find_step(c, GATE_SLUG, GATE).is_some_and(|s| {
                s.get("status").and_then(Value::as_str) == Some("active")
                    && s.pointer(&format!("/metadata/{GATE_RUN}"))
                        .and_then(Value::as_str)
                        == Some(gate_run)
            })
    })
}

/// One judged step write: the body, and the step-metadata keys it was
/// decided on, as they were read — sent in
/// [`crate::step_metadata_write::EXPECT_HEADER`] so the jobs API lands it
/// only on a step that still reads so (409 `step_changed` otherwise,
/// nothing written).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgedWrite {
    /// `PATCH` for the merge door, `PUT` for the status write.
    pub method: &'static str,
    /// The path, without a base.
    pub path: String,
    pub body: Value,
    pub expect: serde_json::Map<String, Value>,
}

/// PURE: a non-green verdict hands the car's `gate` step back — the
/// merge recording the verdict, then a PUT to READY, EACH judged on the
/// gate-run still holding the step. Empty when `gate_run` does not hold
/// this car's gate step.
///
/// READY, NOT BUILD (backlog 68a8726e): `build` completed at launch and
/// a completed step is immutable, so the car waits at its gate for the
/// next launch — which a repair run of the same item makes on the same
/// branch, adopting this car rather than filing a second.
///
/// WHY BOTH WRITES CARRY THE HOLDER (Codex review of car 0d04ee3e, R3).
/// Ownership was judged on the READ alone: a gate launched on the same
/// branch between that read and either write restamps the step to its
/// own gate-run and leaves it ACTIVE, and the unconditioned merge and PUT
/// then set the NEWER gate's step READY with the older gate's verdict. So
/// the merge lands only while `gate_run` still holds the step, and the
/// PUT only while it still does AND the step still carries the verdict
/// the merge wrote — the state that merge produced, judged under the
/// step row's version compare at the PUT's foot.
pub fn gate_return(car: &Value, gate_run: &str, verdict: &str) -> Vec<JudgedWrite> {
    let Some(held) = car_in_gate_run(std::slice::from_ref(car), gate_run) else {
        return Vec::new();
    };
    let (Some(id), Some(car_id)) = (
        find_step(held, GATE_SLUG, GATE)
            .and_then(|s| s.get("id"))
            .and_then(Value::as_str),
        held.get("id").and_then(Value::as_str),
    ) else {
        return Vec::new();
    };
    let last = json!({GATE_RUN: gate_run, "verdict": verdict});
    let holder = serde_json::Map::from_iter([(GATE_RUN.to_string(), json!(gate_run))]);
    let mut merged = holder.clone();
    merged.insert(LAST_VERDICT.to_string(), last.clone());
    vec![
        JudgedWrite {
            method: "PATCH",
            path: format!("/api/jobs/{car_id}/steps/{id}/metadata"),
            body: json!({ LAST_VERDICT: last }),
            expect: holder,
        },
        JudgedWrite {
            method: "PUT",
            path: format!("/api/jobs/{car_id}/steps/{id}"),
            body: json!({"status": "ready"}),
            expect: merged,
        },
    ]
}

/// The expectation a green's gate writes carry on a car whose gate step
/// NO gate-run binds: both binding keys ABSENT (`null` in
/// [`crate::step_metadata_write::EXPECT_HEADER`] reads as absent).
///
/// WHY ABSENCE IS AN EXPECTATION (Codex review of car 0d04ee3e, round 2
/// R2). An unbound gate was finished with UNJUDGED writes, so the
/// unbound-to-bound transition escaped the guard: G1's green read a car
/// still at build, G2 then completed build, stamped its own gate-run and
/// claimed the gate, and G1's writes completed G2's gate with G1's
/// receipt — at the same head too, so no head check could catch it. A
/// binding that APPEARS after the read is a change of hands exactly as a
/// restamp is, and refuses the write the same way.
///
/// And no ending: a car whose run or item ended before any gate is
/// claimed by its ending on this same step ([`ENDED_BEFORE_GATE`]), and
/// is the ending's to finish, not a green's (backlog 366c9fb1).
pub fn unbound_gate() -> serde_json::Map<String, Value> {
    serde_json::Map::from_iter([
        (GATE_RUN.to_string(), Value::Null),
        ("gated_head".to_string(), Value::Null),
        (ENDED_BEFORE_GATE.to_string(), Value::Null),
    ])
}

/// PURE: may a GREEN from `gate_run`, vouching for `head`, finish this
/// car's `gate` step — and on what must the gate step still read at
/// write time? [`unbound_gate`] for a car with no gate binding (a car
/// filed or opened before gate launches stamped one, or one no launch
/// has stamped yet), so a binding that appears after this read refuses
/// the write; the holder and its head when `gate_run` holds the step at
/// `head`; `Err`, naming the holder, when another gate-run holds it or
/// it was launched at another head. EVERY green gate write is judged.
///
/// WHY (Codex review of car 0d04ee3e, R2). A launch restamps an ACTIVE
/// gate step to its own gate-run, and the green path decided Adopt on
/// the branch alone: an older gate's green, delivered after a newer one
/// launched (a re-gate after a repair, or a slow verdict), completed the
/// step with the OLD receipt while the step said the new gate held it —
/// and the new gate's red could then hand back nothing. The non-green
/// path already asked who holds the step; now the green does too.
///
/// A LOST GATE'S RELAUNCH SUCCEEDS IT (backlog 64d417ca). `relaunch_of`
/// is the lost gate-run the green's own gate-run replaces, as the
/// conductor stamps it on a machine relaunch (`lost_gate::RELAUNCH_OF`);
/// a green whose run relaunched the HOLDER finishes the step the holder
/// held, judged on that holder still holding it at its head. Measured
/// 2026-11-10 on car 88645b17: lost cdd2f3d9's hand-back left the step
/// READY naming cdd2f3d9, the relaunch fc5d6d2e went green on the same
/// head, and this answered "held by gate-run cdd2f3d9" — so the car sat
/// about 20 hours off the dock with a valid green. Read off the record
/// the relaunch already carries, at the one moment the binding is
/// consulted, so no extra write sits between the relaunch and its green.
/// A launch that restamped the step after the loss is a change of hands
/// the relaunch does not inherit: its holder is not the lost run.
pub fn green_binding(
    car: &Value,
    gate_run: &str,
    relaunch_of: Option<&str>,
    head: &str,
) -> Result<serde_json::Map<String, Value>, String> {
    let Some(gate) = find_step(car, GATE_SLUG, GATE) else {
        return Ok(unbound_gate());
    };
    if already_done(gate) {
        return Ok(unbound_gate());
    }
    let md = |k: &str| {
        gate.pointer(&format!("/metadata/{k}"))
            .and_then(Value::as_str)
    };
    let Some(holder) = md(GATE_RUN).filter(|h| !h.trim().is_empty()) else {
        // Unbound as read: absence of BOTH keys is what the write expects.
        // A blank `gate_run` left with a `gated_head` beside it is not
        // absence, so it is expected exactly as it was read.
        let mut expect = unbound_gate();
        for k in [GATE_RUN, "gated_head"] {
            if let Some(v) = gate.pointer(&format!("/metadata/{k}")) {
                expect.insert(k.to_string(), v.clone());
            }
        }
        return Ok(expect);
    };
    let car_id = car.get("id").and_then(Value::as_str).unwrap_or("?");
    let short = |s: &str| s[..8.min(s.len())].to_string();
    if holder != gate_run && relaunch_of != Some(holder) {
        return Err(format!(
            "car {}'s gate step is held by gate-run {}, not by gate-run {} — this green is \
             neither that gate's verdict nor its relaunch's, so it finishes nothing (a newer \
             launch restamped the step; its own verdict moves the car)",
            short(car_id),
            short(holder),
            short(gate_run)
        ));
    }
    let launched = md("gated_head");
    if let Some(at) = launched
        && !same_head(at, head)
    {
        return Err(format!(
            "car {}'s gate step was launched at {} by gate-run {}, and this green vouches \
             for {} — a receipt for another head finishes nothing",
            short(car_id),
            short(at),
            short(holder),
            short(head)
        ));
    }
    let mut expect = serde_json::Map::from_iter([(GATE_RUN.to_string(), json!(holder))]);
    expect.insert(
        "gated_head".to_string(),
        launched.map_or(Value::Null, |h| json!(h)),
    );
    Ok(expect)
}

/// Two spellings of one commit: equal, or one an abbreviation (at least
/// seven characters) of the other — a launch may record the sha it
/// resolved in full while a receipt states it shorter, or the reverse.
fn same_head(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short.len() >= 7 && long.starts_with(short)
}

/// The gate-step key keeping the LOST gate-run a relaunch took the step
/// over from, beside [`GATE_RUN`] naming the relaunch whose receipt the
/// step carries (backlog 84e46932).
pub const LOST_GATE_RUN: &str = "lost_gate_run";

/// PURE: a green's two judged writes on the car's `gate` step — the
/// evidence merge, then the status PUT — from [`finish_writes`]' gate
/// entry and the expectation [`green_binding`] returned for `gate_run`.
///
/// A RELAUNCH TAKES THE STEP OVER IN THE SAME MERGE (backlog 84e46932).
/// When the binding accepted the green through `relaunch_of`, the holder
/// it expects is the lost run, not `gate_run`, and the merge then also
/// sets [`GATE_RUN`] to the relaunch and keeps the holder under
/// [`LOST_GATE_RUN`]. Found by review 393c6997 of car 06f1f1bf: the
/// completed step kept naming the lost run beside the relaunch's
/// receipt, which names no gate-run — so a reader following the car's
/// `gate_run` landed on a lost run, and the green one was found only by
/// searching gate-runs for `relaunch_of`. The merge is still judged on
/// the lost run holding the step; the PUT on the state the merge made.
pub fn green_gate_writes(
    car_id: &str,
    gate: &StepWrite,
    expect: &serde_json::Map<String, Value>,
    gate_run: &str,
) -> [JudgedWrite; 2] {
    let mut body = gate.metadata.clone();
    let mut after = expect.clone();
    let lost = expect
        .get(GATE_RUN)
        .and_then(Value::as_str)
        .filter(|h| !h.trim().is_empty() && *h != gate_run);
    if let (Some(lost), Some(m)) = (lost, body.as_object_mut()) {
        for (k, v) in [(GATE_RUN, gate_run), (LOST_GATE_RUN, lost)] {
            m.insert(k.to_string(), json!(v));
            after.insert(k.to_string(), json!(v));
        }
    }
    [
        JudgedWrite {
            method: "PATCH",
            path: gate.merge_path(car_id),
            body,
            expect: expect.clone(),
        },
        JudgedWrite {
            method: "PUT",
            path: gate.status_path(car_id),
            body: gate.status_body.clone(),
            expect: after,
        },
    ]
}

/// PURE: the R8 edge a gate LAUNCH writes onto its gate-run — the car
/// and its `gate` step — read off the car AS IT READS AFTER THE LAUNCH,
/// or `None` unless that gate step names `gate_run` as its holder
/// (design 689d1f7f car 2, backlog a3a367e4).
///
/// READ BACK, NOT ASSUMED. The launch's stamp is judged on its fence and
/// can lose to an ending; the edge is written only once the car is seen
/// to carry the stamp, so a gate-run never points at a step it does not
/// hold. The stamp stays the authority: every verdict write is judged on
/// it ([`gate_return`], [`green_binding`]), and the edge only says where
/// to look.
pub fn gate_edge(car: &Value, gate_run: &str) -> Option<crate::fulfils::Fulfils> {
    let gate = find_step(car, GATE_SLUG, GATE)?;
    let holds = gate
        .pointer(&format!("/metadata/{GATE_RUN}"))
        .and_then(Value::as_str)
        == Some(gate_run);
    if !is_open(car) || already_done(gate) || !holds {
        return None;
    }
    Some(crate::fulfils::Fulfils {
        job: car.get("id").and_then(Value::as_str)?.to_string(),
        step: gate.get("id").and_then(Value::as_str)?.to_string(),
    })
}

/// PURE: the step an R8 edge names on `car`, when `car` is the car the
/// edge names and the step is its `gate` step — whatever the car's
/// status. `Err` says which of those failed.
fn edge_named_gate<'a>(
    car: &'a Value,
    edge: &crate::fulfils::Fulfils,
) -> Result<&'a Value, String> {
    let short = |s: &str| s[..8.min(s.len())].to_string();
    if car.get("id").and_then(Value::as_str) != Some(edge.job.as_str()) {
        return Err(format!(
            "the packet read for car {} is not that car",
            short(&edge.job)
        ));
    }
    let gate = find_step(car, GATE_SLUG, GATE).ok_or_else(|| no_such_step(GATE_SLUG, GATE))?;
    if gate.get("id").and_then(Value::as_str) != Some(edge.step.as_str()) {
        return Err(format!(
            "step {} is not car {}'s `{GATE_SLUG}` step",
            short(&edge.step),
            short(&edge.job)
        ));
    }
    Ok(gate)
}

/// `Err` unless `car` is still open — an ended car is not finished, or
/// handed back, by a verdict.
fn edge_car_open(car: &Value, edge: &crate::fulfils::Fulfils) -> Result<(), String> {
    if is_open(car) {
        return Ok(());
    }
    Err(format!(
        "car {} is no longer open — an ended car is not finished by a verdict",
        &edge.job[..8.min(edge.job.len())]
    ))
}

/// PURE: the step an R8 edge names on `car`, when `car` is the open car
/// the edge names and the step is its `gate` step — the one shape a gate
/// verdict may act on by edge. `Err` says which of those failed.
fn edge_gate_step<'a>(car: &'a Value, edge: &crate::fulfils::Fulfils) -> Result<&'a Value, String> {
    let gate = edge_named_gate(car, edge)?;
    edge_car_open(car, edge)?;
    Ok(gate)
}

/// What a GREEN may do on the car its gate-run's edge names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeGreen {
    /// Finish the car; the `gate` step's writes carry this expectation
    /// (the holder and the head, from [`green_binding`]).
    Finish(serde_json::Map<String, Value>),
    /// The gate step is already completed or skipped — a redelivered
    /// verdict, or one whose step another path finished. Nothing to do.
    Finished,
}

/// PURE: may a green from `gate_run`, vouching for `head`, finish the
/// car its R8 edge names — and on what must the gate step still read?
///
/// STRICTER THAN THE SCAN, ON PURPOSE. [`green_binding`] accepts an
/// UNBOUND gate step, because the scan found the car by its branch and a
/// car filed before launches stamped one is still that branch's car. An
/// edge carries no branch: the only thing that makes this green the
/// step's verdict is the step naming this gate-run (or the lost run it
/// relaunched) as its holder. So an unbound step is refused here, and
/// the edge can never complete a step its gate-run did not hold — the
/// trust boundary this car is marked for (a completed gate step boards).
///
/// AND THE BRANCH THE SCAN MATCHED ON (review 01fe1c71 of car 3a7ab114,
/// finding 4). The edge itself carries no branch, but the gate-run does:
/// `branch` is the one its immutable submission gated, and a car on
/// another branch is refused, so the edge is never less strict than the
/// scan it replaces — whose only way to a car was that branch. Without
/// it, a relaunch naming this car's holder could finish it from another
/// branch at the same head, and carry that branch's park prose onto it.
///
/// A FINISHED STEP IS FINISHED WHATEVER THE CAR DID SINCE (finding 3).
/// The step is judged before the car's status: a green redelivered after
/// its car landed and closed is the green that finished it, so it reads
/// [`EdgeGreen::Finished`] rather than an "ended car" refusal recorded
/// against the very verdict that ended it.
pub fn edge_green(
    car: &Value,
    edge: &crate::fulfils::Fulfils,
    branch: &str,
    gate_run: &str,
    relaunch_of: Option<&str>,
    head: &str,
) -> Result<EdgeGreen, String> {
    let gate = edge_named_gate(car, edge)?;
    let carries = car.pointer("/metadata/branch").and_then(Value::as_str);
    if carries != Some(branch) {
        return Err(format!(
            "car {} carries branch {}, and this gate-run gated {branch} — an edge finishes only \
             the car of the branch its gate-run gated",
            &edge.job[..8.min(edge.job.len())],
            carries.unwrap_or("(none)")
        ));
    }
    if already_done(gate) {
        return Ok(EdgeGreen::Finished);
    }
    edge_car_open(car, edge)?;
    let expect = green_binding(car, gate_run, relaunch_of, head)?;
    if expect.get(GATE_RUN).is_none_or(Value::is_null) {
        return Err(format!(
            "car {}'s gate step names no gate-run — an edge is a locator, not a holder, so this \
             green finishes nothing",
            &edge.job[..8.min(edge.job.len())]
        ));
    }
    Ok(EdgeGreen::Finish(expect))
}

/// PURE: the hand-back a NON-GREEN verdict owes the car its gate-run's
/// R8 edge names — [`gate_return`]'s two judged writes, or none when
/// this gate-run does not hold the step (a newer launch restamped it, or
/// a redelivery finds it already handed back). `Err` when the edge does
/// not name an open car's gate step.
///
/// NEVER A COMPLETION. A red, lost, refused or withdrawn verdict puts the
/// step back to READY with the verdict beside it; nothing on this path
/// completes the step, which is what keeps a red off the dock.
pub fn edge_return(
    car: &Value,
    edge: &crate::fulfils::Fulfils,
    gate_run: &str,
    verdict: &str,
) -> Result<Vec<JudgedWrite>, String> {
    edge_gate_step(car, edge)?;
    Ok(gate_return(car, gate_run, verdict))
}

/// Which car a dispatched run's admission or gate launch acts on.
#[derive(Debug, PartialEq)]
pub enum RunCar<'a> {
    /// The run's own car, found by its `agent_run` — on ANOTHER branch
    /// than the one asked about, so the caller re-points it first.
    Run(&'a Value),
    /// The branch's building car — the run's own, a hand-opened one, or
    /// one an earlier run of the SAME change admitted (a repair).
    Branch(&'a Value),
    /// Neither: the run has no building car and the branch has none.
    Neither,
}

/// PURE: the car a run acts on, by STABLE IDENTITY FIRST — the building
/// car whose `agent_run` is `run` — and only then by `branch`; or why
/// none may be chosen.
///
/// WHY IDENTITY BEFORE BRANCH (Codex review of car 0d04ee3e, R1). A
/// branch is the one thing a builder changes under a run: start on
/// feat/a, rename to feat/b, run `--started` again, and a branch-first
/// admission found no car for feat/b and filed a second with the same
/// run and item, while a gate on feat/b moved that second car and left
/// the first at build. Asked by the run first, the second start
/// re-points the one car and the gate moves it.
///
/// REFUSED, NEVER GUESSED: two building cars carrying one run (the twin
/// this exists to stop, already made); the run's car on another branch
/// while this branch has a live car of its own (re-pointing would make
/// two); and a building car on this branch that ANOTHER run of ANOTHER
/// change admitted — stamping its gate would hand some other build's car
/// to this gate. Another run of the SAME change (`item`) is a repair and
/// is the branch's car as before.
pub fn run_car_for<'a>(
    cars: &'a [Value],
    run: Option<&str>,
    item: Option<&str>,
    branch: &str,
) -> Result<RunCar<'a>, String> {
    let id = |c: &Value| {
        let id = c.get("id").and_then(Value::as_str).unwrap_or("?");
        id[..8.min(id.len())].to_string()
    };
    let md = |c: &'a Value, k: &str| -> Option<&'a str> {
        c.pointer(&format!("/metadata/{k}"))
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
    };
    let mine: Vec<&Value> = match run {
        Some(run) => cars
            .iter()
            .filter(|c| is_building(c) && md(c, "agent_run") == Some(run))
            .collect(),
        None => Vec::new(),
    };
    if mine.len() > 1 {
        return Err(format!(
            "run {} already has {} building cars ({}) — one run is one car, so this refuses \
             to choose; abandon the twin before going on",
            run.unwrap_or_default(),
            mine.len(),
            mine.iter().map(|c| id(c)).collect::<Vec<_>>().join(", ")
        ));
    }
    // THE WHOLE BRANCH CANDIDATE SET, NOT ITS FIRST ROW (Codex review of
    // car 0d04ee3e, round 2 R1). Two building cars on one branch is the
    // twin already made, and choosing `find`'s first made the answer
    // depend on list order: a repair run adopted whichever listed first
    // and stamped itself on it, leaving the other at build. So the set is
    // read whole and two is refused by name, for every run — the owner
    // of one of them included, since "the branch's car" no longer names
    // one car.
    let on_branch: Vec<&Value> = cars
        .iter()
        .filter(|c| {
            c.pointer("/metadata/branch").and_then(Value::as_str) == Some(branch) && is_building(c)
        })
        .collect();
    if on_branch.len() > 1 {
        return Err(format!(
            "{branch} has {} building cars ({}) — a branch is one car, so this refuses to \
             choose; abandon the twin before going on",
            on_branch.len(),
            on_branch
                .iter()
                .map(|c| id(c))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    match (mine.first().copied(), on_branch.first().copied()) {
        (Some(m), Some(b)) if m.get("id") == b.get("id") => Ok(RunCar::Branch(b)),
        (Some(m), Some(b)) => Err(format!(
            "run {}'s car {} is on {}, and {branch} has its own building car {} — refusing to \
             choose between them",
            run.unwrap_or_default(),
            id(m),
            md(m, "branch").unwrap_or("?"),
            id(b)
        )),
        (Some(m), None) => match open_car_for(cars, branch) {
            Some(other) => Err(format!(
                "run {}'s car {} is on {}, and {branch} already has live car {} — re-pointing \
                 the run's car would give the branch two",
                run.unwrap_or_default(),
                id(m),
                md(m, "branch").unwrap_or("?"),
                id(other)
            )),
            None => Ok(RunCar::Run(m)),
        },
        (None, Some(b)) => {
            let owner = md(b, "agent_run");
            let same_change = item.is_some() && md(b, "backlog_item") == item;
            match owner {
                Some(owner) if Some(owner) != run && !same_change => Err(format!(
                    "{branch}'s building car {} was admitted by run {owner} for {} — another \
                     run of another change; this one ({}) does not move it",
                    id(b),
                    md(b, "backlog_item").unwrap_or("no item"),
                    run.unwrap_or("no run")
                )),
                _ => Ok(RunCar::Branch(b)),
            }
        }
        (None, None) => Ok(RunCar::Neither),
    }
}

/// The gate-step key naming the run a REPAIR adopted the car for —
/// written by `boss dispatch --started` when a later run of the same
/// change takes a car an earlier run admitted, judged on
/// [`ENDED_BEFORE_GATE`] being absent ([`adoption`]).
pub const BUILD_RUN: &str = "build_run";

/// THE ONE FENCE between a car's two ways off `build` (backlog 366c9fb1;
/// Codex review of car 0d04ee3e, R4b): the gate-step key an ending of an
/// UNLAUNCHED car claims it under — `{by, id, outcome, at}`, which run or
/// item ended, how, and when ([`ending_claim`]).
///
/// A gate launch's FIRST write stamps `gate_run` on the same step judged
/// on this key ABSENT ([`launch_fence`]); an ending's first write stamps
/// this key judged on `gate_run` ABSENT. One step row, two mutually
/// exclusive expectations: the row's version compare lets exactly one
/// land, and the loser has written nothing — a launch completes `build`
/// only after its stamp landed, and an ending touches `abandoned` only
/// after its claim did.
pub const ENDED_BEFORE_GATE: &str = "ended_before_gate";

/// The job-metadata key saying why an unlaunched car was abandoned, in
/// words, beside the `abandoned` marker its terminal waits on.
pub const ABANDONED_WHY: &str = "abandoned_why";

/// The expectation a gate launch's first write — the gate-step stamp —
/// carries: no ending has claimed the car ([`ENDED_BEFORE_GATE`]).
pub fn launch_fence() -> serde_json::Map<String, Value> {
    serde_json::Map::from_iter([(ENDED_BEFORE_GATE.to_string(), Value::Null)])
}

/// The ending claim this car's gate step carries, if one landed — the
/// decided ending any reader that finds it finishes ([`ending_rest`]).
pub fn ending_of(car: &Value) -> Option<&Value> {
    find_step(car, GATE_SLUG, GATE)?
        .pointer(&format!("/metadata/{ENDED_BEFORE_GATE}"))
        .filter(|v| v.is_object())
}

/// PURE: the judged write a repair run's adoption of `car` sends FIRST —
/// `(gate step id, body, expect)`: `{build_run: run}` onto the gate step,
/// landing only while no ending has claimed the car. `None` when the car
/// has no gate step id to bind on.
pub fn adoption(car: &Value, run: &str) -> Option<(String, Value, serde_json::Map<String, Value>)> {
    Some((
        step_id_for(car, GATE_SLUG, GATE)?,
        json!({ BUILD_RUN: run }),
        launch_fence(),
    ))
}

/// What ended, so a car built for it and never gated may end too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended<'a> {
    /// An `agent-run` closed `outcome` (refused, died) — its admitted car.
    Run { run: &'a str, outcome: &'a str },
    /// A `backlog-item` closed `outcome` (declined, stale, duplicate) —
    /// every car building for it.
    Item { item: &'a str, outcome: &'a str },
}

impl Ended<'_> {
    /// The car-metadata edge that names what ended.
    pub fn edge(&self) -> (&'static str, &str) {
        match self {
            Self::Run { run, .. } => ("agent_run", run),
            Self::Item { item, .. } => (BACKLOG_ITEM, item),
        }
    }
}

/// One plain write an ending still owes, in the order it is owed.
#[derive(Debug, Clone, PartialEq)]
pub struct EndingWrite {
    /// `PATCH` for a merge door, `PUT` for the status write.
    pub method: &'static str,
    /// The path, without a base.
    pub path: String,
    pub body: Value,
}

/// PURE: may this car end because `ended` ended — and if so, the judged
/// CLAIM that decides it, sent first. `Err`, naming why, leaves it alone.
///
/// WHY (backlog 366c9fb1; design 689d1f7f car 1). `boss dispatch
/// --started` admits a car at build. A builder that stops without
/// building ends its RUN `refused`, one that goes silent is ended `died`,
/// and an ITEM can close declined, stale or duplicate under its car;
/// nothing then moved the car, which stood open at `build` for ever. Only
/// a car no gate ever ran for ends here: a launch stamps `gate_run` FIRST,
/// and a gated car waits at its gate for the repair that re-gates it (a
/// red ends its run `refused` too, by design).
///
/// LEFT ALONE, NAMED: a closed, boarded or past-its-gate car; one already
/// claimed by an ending (that ending is FINISHED, by [`ending_rest`], not
/// claimed twice); a gate step a launch stamped; a completed `build`; a
/// car whose edge no longer names what ended, or whose gate step a repair
/// bound to another run; a car pinned to a version with no `abandoned`
/// terminal, or whose terminal is already taken.
pub fn ending_claim(
    car: &Value,
    ended: Ended<'_>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<JudgedWrite, String> {
    let id = car.get("id").and_then(Value::as_str).unwrap_or("?");
    let short = &id[..8.min(id.len())];
    if !is_building(car) {
        return Err(format!(
            "car {short} is not building (closed, aboard a train, or past its gate)"
        ));
    }
    let (edge, named) = ended.edge();
    if car
        .pointer(&format!("/metadata/{edge}"))
        .and_then(Value::as_str)
        != Some(named)
    {
        return Err(format!(
            "car {short}'s `{edge}` no longer names {named} — another run adopted it, or it \
             answers another change"
        ));
    }
    if ending_of(car).is_some() {
        return Err(format!(
            "car {short} is already claimed by an ending — it is finished, not claimed twice"
        ));
    }
    let Some(gate) = find_step(car, GATE_SLUG, GATE) else {
        return Err(no_such_step(GATE_SLUG, GATE));
    };
    let gate_md = |k: &str| gate.pointer(&format!("/metadata/{k}"));
    if let Some(holder) = gate_md(GATE_RUN).filter(|v| !v.is_null()) {
        return Err(format!(
            "car {short}'s gate was launched (gate-run {}) — a gated car waits at its gate \
             for the repair that re-gates it",
            holder.as_str().unwrap_or("?")
        ));
    }
    if find_step(car, BUILD_SLUG, BUILD).is_some_and(already_done) {
        return Err(format!(
            "car {short}'s build is already handed in — it is not an unlaunched car"
        ));
    }
    let bound = gate_md(BUILD_RUN).cloned().unwrap_or(Value::Null);
    if let Ended::Run { run, .. } = ended
        && bound.as_str().is_some_and(|b| b != run)
    {
        return Err(format!(
            "car {short}'s gate is bound to run {} — a repair adopted it",
            bound.as_str().unwrap_or("?")
        ));
    }
    let Some(gate_id) = gate.get("id").and_then(Value::as_str) else {
        return Err(format!("car {short}'s `{GATE_SLUG}` step has no id"));
    };
    abandoned_step(car)?;
    let (by, what, how) = match ended {
        Ended::Run { run, outcome } => ("run", run, outcome),
        Ended::Item { item, outcome } => ("item", item, outcome),
    };
    Ok(JudgedWrite {
        method: "PATCH",
        path: format!("/api/jobs/{id}/steps/{gate_id}/metadata"),
        body: json!({ ENDED_BEFORE_GATE: {"by": by, "id": what, "outcome": how, "at": stamp(now)} }),
        expect: serde_json::Map::from_iter([
            (GATE_RUN.to_string(), Value::Null),
            (ENDED_BEFORE_GATE.to_string(), Value::Null),
            (BUILD_RUN.to_string(), bound),
        ]),
    })
}

/// The car's `abandoned` terminal, while it can still be taken.
fn abandoned_step(car: &Value) -> Result<&Value, String> {
    let id = car.get("id").and_then(Value::as_str).unwrap_or("?");
    let short = &id[..8.min(id.len())];
    let Some(step) = find_step(
        car,
        crate::car_retire::ABANDONED_SLUG,
        crate::car_retire::ABANDONED,
    ) else {
        return Err(format!(
            "car {short} has no `abandoned` terminal on its pinned version"
        ));
    };
    match step.get("status").and_then(Value::as_str) {
        Some("pending" | "ready" | "completed") => Ok(step),
        other => Err(format!(
            "car {short}'s `abandoned` terminal is {} — it cannot be taken",
            other.unwrap_or("?")
        )),
    }
}

/// PURE: the writes an ending CLAIMED on this car still owes, read off
/// the claim and the car as they stand — evidence onto `abandoned`, the
/// marker its `ready_when` waits on, then its status. Empty when the
/// terminal is already completed.
///
/// RESUMABLE BY CONSTRUCTION (Codex review of car 0d04ee3e, R4a). Every
/// word and stamp is derived from the CLAIM, never from the resumer, so a
/// retry after any one write — the dispatcher's redelivery, a repair
/// whose adoption found the claim, a gate launch whose stamp lost to it —
/// sends the same bytes and skips what already landed. The claim itself
/// is the only judged write; after it no launch or adoption can land, so
/// nothing a resumer sends can contend.
pub fn ending_rest(car: &Value) -> Result<Vec<EndingWrite>, String> {
    let id = car.get("id").and_then(Value::as_str).unwrap_or("?");
    let Some(claim) = ending_of(car) else {
        return Err(format!(
            "car {} carries no ending claim",
            &id[..8.min(id.len())]
        ));
    };
    let terminal = abandoned_step(car)?;
    if terminal.get("status").and_then(Value::as_str) == Some("completed") {
        return Ok(Vec::new());
    }
    let terminal_id = terminal
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("car {id}'s `abandoned` step has no id"))?;
    let c = |k: &str| claim.get(k).and_then(Value::as_str).unwrap_or("?");
    let why = format!(
        "the {} {} that this car was admitted for ended {} before any gate ran for it, so \
         the car ends with it rather than standing at build for ever",
        if c("by") == "item" {
            "backlog item"
        } else {
            "agent-run"
        },
        c("id"),
        c("outcome"),
    );
    let mut owed = Vec::new();
    if terminal
        .pointer("/metadata/evidence")
        .and_then(Value::as_str)
        != Some(why.as_str())
    {
        owed.push(EndingWrite {
            method: "PATCH",
            path: format!("/api/jobs/{id}/steps/{terminal_id}/metadata"),
            body: json!({ "evidence": why, "completed_at": c("at") }),
        });
    }
    let md = |k: &str| {
        car.pointer(&format!("/metadata/{k}"))
            .and_then(Value::as_str)
    };
    if md(crate::car_retire::ABANDONED_MARKER) != Some("true") || md(ABANDONED_WHY) != Some(&why) {
        owed.push(EndingWrite {
            method: "PATCH",
            path: format!("/api/jobs/{id}/metadata"),
            body: json!({ crate::car_retire::ABANDONED_MARKER: "true", ABANDONED_WHY: why }),
        });
    }
    owed.push(EndingWrite {
        method: "PUT",
        path: format!("/api/jobs/{id}/steps/{terminal_id}"),
        body: json!({"status": "completed"}),
    });
    Ok(owed)
}

/// THE CAR CARRIES ITS PROBE. `boss prove` records a proof as a
/// command that ran plus the string it had to print; until 2026-09-08
/// that command was authored by an operator AFTER the car landed —
/// 43 landed cars sat at `proven: ready` for 2.5 days waiting for one
/// (backlog 28ac45ab). The builder who parks the car knows what the
/// change does and can write the probe then, so the park intent
/// carries it and the car records it under these keys, in exactly the
/// shape `boss prove --from-car` and the arrival rule re-run. ONE
/// definition of the key names, read by the gate verb, the auto-park
/// handler and the prove verb, so the writer and the readers cannot
/// drift (CLAUDE.md 9a).
pub const PROOF_PROBE: &str = "proof_probe";
/// The string the recorded probe must print — never optional: a probe
/// with no expectation is `echo hi`, which exits 0 too.
pub const PROOF_EXPECT: &str = "proof_expect";
/// An EVENT-BOUND car has no mechanical probe: what proves it is a
/// thing that has to happen (a stalled train, a red gate, an operator's
/// cancel). Recorded as prose naming the event and how to prove it
/// when it fires — the `proof_finding` vocabulary operators wrote by
/// hand on 2026-09-08 — so the yard can count it as waiting rather
/// than forgotten, and the arrival rule knows to leave it alone.
pub const PROOF_EVENT: &str = "proof_event";
/// THE ATTEMPT RECORD — what every probe door (`boss prove`, the
/// dispatcher's car-probe sweep, a host's environment refusal) writes
/// on the car when a run settled nothing. It is replaced by the next
/// unsettled run and NEVER cleared by a proof, so a closed car carrying
/// one is a car whose proof did not settle on its first run: a failure,
/// a not-yet wait, or a run that could not run or was refused — the
/// caveat the retros count (backlog 78a42b2a: 364 cars carried it on
/// 2026-10-21, 358 of them closed with `proven` completed). It is NOT a
/// count of failed proofs: of the 65 IT cars closed that week carrying
/// one, 38 held a not-yet wait and 27 a failure (review b4bae4e7), so a
/// reader splits the records by `exit` / `not_yet` before calling any a
/// failure. One spelling for the writers and that count (CLAUDE.md §9a).
pub const PROOF_ATTEMPT: &str = "proof_attempt";

/// The proof intent a car carries in its metadata: the probe + expect
/// pair, or the event that stands in for one. Absent keys are omitted
/// (never nulled), so merging this into a car body or a metadata
/// PATCH adds what was stated and touches nothing else.
pub fn proof_intent(
    probe: Option<&str>,
    expect: Option<&str>,
    event: Option<&str>,
) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    let mut put = |k: &str, v: Option<&str>| {
        if let Some(v) = v.filter(|s| !s.trim().is_empty()) {
            m.insert(k.to_string(), json!(v));
        }
    };
    put(PROOF_PROBE, probe);
    put(PROOF_EXPECT, expect);
    put(PROOF_EVENT, event);
    m
}

/// The proof intent a gate-run's park keys state (`park_probe`,
/// `park_expect`, `park_proof_event`), as the car carries it — empty
/// for a bare re-gate. ONE reading for both doors that refresh a car
/// from a green: the auto-park handler and `boss rerail --finish`.
/// Until 2026-09-28 only the handler read it, so `--finish` kept the
/// first gate's probe whatever the re-gate carried; car bfb219b4 was
/// then proven FAILING on correct code, because the review's fold had
/// changed the line its first probe grepped (backlog 79a17c7a).
pub fn proof_intent_of_park(
    gate_run_md: &serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    let s = |k: &str| gate_run_md.get(k).and_then(Value::as_str);
    proof_intent(s(PARK_PROBE), s(PARK_EXPECT), s(PARK_PROOF_EVENT))
}

/// What every park door says when a car would stand at the dock owing a
/// proof nothing can settle. ONE text for `boss gate`, `boss park`,
/// `boss rerail --finish` and the auto-park handler (CLAUDE.md §9a), so
/// the remedy a builder is told to type cannot differ between doors.
pub const UNPROVEN_PARK: &str = "a car parks carrying neither a probe nor a proof event, so \
     once it lands nothing can prove it: the shed reads it owed for good and proven-in-prod \
     means nothing for it. Measured 2026-11-08 (backlog 031a3ca9): 65 of 81 landed cars owed a \
     proof carried neither, because no park door refused them.\n\n\
     These are boss gate's flags (boss park and boss rerail take none): re-gate the branch \
     with the probe the forge runs, boss gate <branch> --park-probe '<shell>' with \
     --park-expect '<the string it prints>' (park-file keys `probe` and `expect`), or, when \
     only something that has to happen can prove the car, --park-proof-event '<the event, and \
     how to prove it when it fires>' (park-file key `proof_event`). A car that already \
     carries a probe or an event keeps it, so a re-gate of it need not restate either \
     (backlog 92a51856).";

/// THE CAR AT THE DOCK CARRIES ITS PROOF (backlog 031a3ca9). `Some` with
/// [`UNPROVEN_PARK`] when a park would leave the car with neither a
/// probe nor an event: the park itself states none (`proof`, in the
/// car's keys — [`proof_intent`] / [`proof_intent_of_park`]) AND the car
/// it lands on, if one exists already, carries none either. A re-gate
/// that states nothing is fine on a car that already carries its probe
/// — the dock's own re-gate stamps only a summary, by design — and is
/// refused on one that does not, because refreshing it would vouch for
/// a car nothing can prove.
///
/// An expect without a probe is not a proof: `boss gate` refuses that
/// pair, and the arrival rule cannot run a string.
pub fn unproven_park(
    car: Option<&Value>,
    proof: &serde_json::Map<String, Value>,
) -> Option<String> {
    let proves = |m: &serde_json::Map<String, Value>| {
        [PROOF_PROBE, PROOF_EVENT].iter().any(|k| {
            m.get(*k)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty())
        })
    };
    let car_proves = car
        .and_then(|c| c.get("metadata"))
        .and_then(Value::as_object)
        .is_some_and(proves);
    (!proves(proof) && !car_proves).then(|| UNPROVEN_PARK.to_string())
}

/// THE NOT-YET STREAK (backlog adef5ddf). A car's `proof_attempt` is
/// REPLACED on every run, so until 2026-09-23 it said what the last run
/// answered and nothing about how long it had been answering it. `exit
/// 75` means "early, not wrong", and a probe that can NEVER pass says
/// exactly that too: car b8c4267f greps a literal a later car removed on
/// 9a grounds, car 52e0287e takes the last of four matches where the
/// call is the second. Both were counted as patiently waiting and
/// rechecked hourly — measured over the 1812 ops-requests on record
/// (2026-09-17 to 09-23): six open cars at 86 to 88 consecutive
/// not-yets each, spanning 97h to 134h, and no probe run ever answered
/// them otherwise. The exit code cannot tell starving from waiting; the
/// length of the streak is the only signal there is, so each attempt now
/// carries where its streak began and how many runs it holds.
///
/// Written by the doors that record an attempt
/// (`boss prove`'s `attempt_json`), read by [`not_yet_streak`].
pub const NOT_YET_SINCE: &str = "not_yet_since";
/// How many consecutive runs of the same probe have answered not-yet,
/// this one included. See [`NOT_YET_SINCE`].
pub const NOT_YET_RUNS: &str = "not_yet_runs";

/// Did this attempt defer completion? An observed exit wins over any
/// convenience flag. A recorded environment refusal also preserves retry
/// accounting, but the shed still judges that unexecuted probe unread.
pub fn attempt_said_not_yet(attempt: &Value) -> bool {
    match attempt.get("exit").and_then(Value::as_i64) {
        Some(exit) => exit == 75,
        None => {
            attempt.get("environment").and_then(Value::as_bool) == Some(true)
                && attempt.get("not_yet").and_then(Value::as_bool) == Some(true)
        }
    }
}

/// Where a not-yet run's streak began and how many runs it now holds,
/// given the car's PRIOR attempt: `(since, runs)`, for a run at `at` of
/// `probe`. The streak continues only across not-yets of the SAME probe
/// text — a corrected probe is the repair for a starved one, and must
/// not inherit its four days. A prior record written before these keys
/// existed dates the streak from its own `at`, the earliest not-yet this
/// door can vouch for.
pub fn carried_not_yet_streak(prior: Option<&Value>, probe: &str, at: &str) -> (String, u64) {
    let continuing = prior.filter(|p| {
        attempt_said_not_yet(p) && p.get("probe").and_then(Value::as_str) == Some(probe)
    });
    match continuing {
        Some(p) => {
            let since = p
                .get(NOT_YET_SINCE)
                .and_then(Value::as_str)
                .or_else(|| p.get("at").and_then(Value::as_str))
                .unwrap_or(at);
            let runs = p
                .get(NOT_YET_RUNS)
                .and_then(Value::as_u64)
                .filter(|n| *n > 0)
                .unwrap_or(1);
            (since.to_string(), runs + 1)
        }
        None => (at.to_string(), 1),
    }
}

/// A car's not-yet streak, read back: how many hours lie between its
/// first and its latest not-yet, and how many runs answered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotYetStreak {
    pub hours: i64,
    pub runs: u64,
}

/// The streak on a car's metadata, if its last run answered not-yet.
///
/// Measured between two RUNS, never against the clock: a recheck that
/// stopped firing must not age a streak nobody is asking. And `None`
/// when the car's recorded probe is no longer the text that ran, so a
/// probe corrected by a metadata PATCH stops reading as starved at once
/// rather than at the next hourly write.
pub fn not_yet_streak(md: &Value) -> Option<NotYetStreak> {
    let attempt = md.get(PROOF_ATTEMPT)?;
    if !attempt_said_not_yet(attempt) {
        return None;
    }
    let ran = attempt.get("probe").and_then(Value::as_str);
    let recorded = md.get(PROOF_PROBE).and_then(Value::as_str);
    if let (Some(ran), Some(recorded)) = (ran, recorded)
        && ran != recorded
    {
        return None;
    }
    let instant = |k: &str| {
        attempt
            .get(k)
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
    };
    let last = instant("at")?;
    let since = instant(NOT_YET_SINCE).unwrap_or(last);
    let runs = attempt
        .get(NOT_YET_RUNS)
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .unwrap_or(1);
    Some(NotYetStreak {
        hours: (last - since).num_hours(),
        runs,
    })
}

/// WHAT THE CAR SAYS IT WAITS ON (backlog b461341d). Duration alone
/// cannot tell stuck from patient: the operator triage of the six cars
/// adef5ddf's streak measured past its bound (b8c4267f, 4b05fe3e,
/// f71a3c90, b94cb42f, 59398050, 7f01b854, 2026-09-23) found ALL SIX
/// honestly waiting on the world — a publish failure that has not
/// happened, a tenant publish, a red crawl, a release David has not
/// opened, a destructive prune that is his call, a real Stripe charge —
/// and none with a wrong probe. So about three days after that bound
/// converged the shed would have called six honest waits "ours to read",
/// the false signal the label exists to prevent, pointed the other way.
///
/// So the car SAYS what it waits on, as `{"on": <prose naming the event
/// or the actor>, "seen": <optional shell text>}`. `seen` is a second,
/// smaller probe, run by the same door and on the same host as the
/// proof probe whenever that one answers not-yet. Exit 0 means the event
/// is seen. A successful negative read exits 1 with JSON stdout containing
/// `"observation":"unseen"`; a legacy nonzero or failed read is unread.
/// It is what turns the declaration from
/// a belief into something the record can contradict: a probe still
/// saying not-yet AFTER its own declared event was seen is the true
/// "ours to read", at any streak length. Written by `boss car waits-on`
/// (or the metadata PATCH it wraps), read by [`starved`].
pub const WAITS_ON: &str = "waits_on";
/// On a not-yet `proof_attempt`: when the car's declared `seen` check
/// first exited 0 in an unbroken run of sightings, or `null`. Carried by
/// [`carried_seen_at`].
pub const WAITS_ON_SEEN_AT: &str = "waits_on_seen_at";

/// A car's declared wait, read back. `None` for no declaration, and for
/// one that names nothing — a blank `on` must not silence the label by
/// merely being present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitsOn {
    pub on: String,
    pub seen: Option<String>,
}

fn non_blank(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn waits_on(md: &Value) -> Option<WaitsOn> {
    let w = md.get(WAITS_ON)?;
    Some(WaitsOn {
        on: non_blank(w.get("on"))?,
        seen: non_blank(w.get("seen")),
    })
}

/// The declaration in the one shape [`waits_on`] reads — what the verb
/// PATCHes, and what an operator writing the PATCH by hand should copy.
pub fn waits_on_value(on: &str, seen: Option<&str>) -> Value {
    let seen = seen.map(str::trim).filter(|s| !s.is_empty());
    json!({"on": on.trim(), "seen": seen})
}

/// A written declaration MERGED into the one the car carries (backlog
/// e9b164a1 piece 3): each field `update` carries replaces that field,
/// every other field `existing` declares is kept. `None` when the result
/// names no `on` — a declaration of nothing, which neither writer may
/// record. One definition for both writers, `boss car waits-on` and the
/// auto-park handler's copy of `--park-waits-on`, because the object is
/// wider than its first two fields now: an `owner` and a patience
/// written by hand must survive a writer that re-states only `on`, and
/// the metadata door merges top-level keys only.
pub fn merge_waits_on(existing: Option<&Value>, update: &Value) -> Option<Value> {
    let mut merged = existing
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    merged.extend(update.as_object()?.clone());
    let merged = Value::Object(merged);
    waits_on(&json!({ WAITS_ON: merged.clone() }))?;
    Some(merged)
}

/// When the declared event was first seen, for a run that `seen` it (or
/// not): the prior attempt's sighting if it had one, else this run's
/// instant; `None` for a run that did not see it — a sighting that stops
/// is not a sighting.
pub fn carried_seen_at(prior: Option<&Value>, seen: bool, at: &str) -> Option<String> {
    if !seen {
        return None;
    }
    Some(
        prior
            .and_then(|p| p.get(WAITS_ON_SEEN_AT))
            .and_then(Value::as_str)
            .unwrap_or(at)
            .to_string(),
    )
}

/// Why a not-yet car is ours to read rather than the world's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Starved {
    /// No declared wait, and the probe has said not-yet without a break
    /// past `regions::NOT_YET_STARVED_HOURS` — adef5ddf's rule, which is
    /// all the record can say of a car that never said what it waits on.
    Undeclared(NotYetStreak),
    /// The car declared what it waits on, its `seen` check found that
    /// in the record, and the probe STILL said not-yet.
    SeenWhileNotYet { on: String, seen_at: String },
}

/// Is this car's not-yet ours to read? One definition, read by the
/// shed and by `boss orient`. A DECLARED wait not yet seen is never
/// starved, however long — its patience is stated, and it becomes ours
/// the moment the record holds what it named.
pub fn starved(md: &Value) -> Option<Starved> {
    let streak = not_yet_streak(md)?;
    match waits_on(md) {
        None => (streak.hours >= crate::regions::NOT_YET_STARVED_HOURS)
            .then_some(Starved::Undeclared(streak)),
        Some(w) => md
            .pointer(&format!("/proof_attempt/{WAITS_ON_SEEN_AT}"))
            .and_then(Value::as_str)
            .map(|seen_at| Starved::SeenWhileNotYet {
                on: w.on,
                seen_at: seen_at.to_string(),
            }),
    }
}

/// WHOSE MOVE A DECLARED WAIT IS (backlog 3881f5c9). Inside `waits_on`:
/// `"world"` for an event nobody here can cause, or the id of the actor
/// whose act it is. DECLARED, never inferred — `on` prose that says
/// "David opens it" names no owner, because a reader that guessed an
/// owner out of prose would be the mostly-sure the shed exists to
/// refuse. No writer sets it yet (`boss car waits-on` writes only `on`
/// and `seen`); until one does, it is written through the metadata
/// PATCH, and a car without it reads as ours.
pub const WAITS_ON_OWNER: &str = "owner";
/// Inside `waits_on`, optional: how many hours from the car's opening
/// the owner's move may take before the wait is ours again. A positive
/// integer or nothing — patience is the car's to state, not a default.
pub const WAITS_ON_MAX_WAIT_HOURS: &str = "max_wait_hours";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOwner {
    World,
    Actor(String),
}

impl std::fmt::Display for WaitOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WaitOwner::World => f.write_str("the world"),
            WaitOwner::Actor(a) => f.write_str(a),
        }
    }
}

/// The owner a car's `waits_on` declares, or `None` for none or blank.
pub fn wait_owner(md: &Value) -> Option<WaitOwner> {
    let owner = non_blank(md.get(WAITS_ON)?.get(WAITS_ON_OWNER))?;
    Some(if owner.eq_ignore_ascii_case("world") {
        WaitOwner::World
    } else {
        WaitOwner::Actor(owner)
    })
}

/// A wait that is someone else's move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedWait {
    pub owner: WaitOwner,
    pub on: String,
    pub max_wait_hours: Option<i64>,
}

impl OwnedWait {
    /// Past the patience the car declared, measured on its age.
    pub fn overdue(&self, age_hours: i64) -> bool {
        self.max_wait_hours.is_some_and(|max| age_hours > max)
    }

    /// Compare the recorded bound without rounding away a missed check.
    pub fn overdue_seconds(&self, age_seconds: i64) -> bool {
        self.max_wait_hours
            .is_some_and(|max| i128::from(age_seconds) > i128::from(max) * 3600)
    }
}

/// An executed observer's result. Exit zero is a sighting. A negative
/// reading requires exit one AND the executed observer's JSON stdout
/// `{"observation":"unseen"}`. Other exits, and legacy nonzero outputs,
/// cannot distinguish a negative observation from a failed read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitObservation {
    Seen,
    Unseen,
    Unread,
}

pub fn wait_observation(attempt: &Value) -> WaitObservation {
    if attempt
        .get("waits_on_missing_tools")
        .is_some_and(|tools| tools.as_array().is_none_or(|tools| !tools.is_empty()))
        || ["waits_on_refusal", "waits_on_error"]
            .iter()
            .any(|field| attempt.get(*field).is_some_and(|value| !value.is_null()))
    {
        return WaitObservation::Unread;
    }
    match attempt.get("waits_on_exit").and_then(Value::as_i64) {
        Some(0) => WaitObservation::Seen,
        Some(1)
            if attempt.get("waits_on_observation").and_then(Value::as_str) == Some("unseen") =>
        {
            WaitObservation::Unseen
        }
        _ => WaitObservation::Unread,
    }
}

/// A declared observation requires an executed, distinguishable outcome.
/// Declaring a command alone cannot establish that the event is unseen.
pub fn wait_observer_unread(md: &Value) -> bool {
    waits_on(md).is_some_and(|w| w.seen.is_some())
        && wait_observation(&md[PROOF_ATTEMPT]) == WaitObservation::Unread
}

/// Is this car's wait someone else's move? Only when all four hold: it
/// DECLARED the wait, it names an OWNER, a WORLD wait has something
/// that OBSERVES it (a `seen` check), and the event has not been seen
/// while the probe says not-yet ([`starved`] is `None`). Anything short
/// of that is ours — the shed's troubled colour means exactly that set.
///
/// WHY ONLY THE WORLD NEEDS AN OBSERVER (3881f5c9, fix shape (2)).
/// Nothing but a `seen` check can say a Stripe charge arrived, so an
/// unobserved world wait is ours to observe. A named actor's act is
/// that actor's next move whether or not a check watches for it — the
/// dev-door car, waiting on David's SSH CA ceremony with no probe at
/// all, troubled the shed alone on 2026-09-24 though its owner was
/// declared as data. Its bound is the patience the car declares, and a
/// check that does exist still turns a seen-while-not-yet into ours.
pub fn owned_wait(md: &Value) -> Option<OwnedWait> {
    let w = waits_on(md)?;
    let owner = wait_owner(md)?;
    if owner == WaitOwner::World && w.seen.is_none() {
        return None;
    }
    // The proof door explicitly records null when its observer was
    // refused, could not execute, or lacked a tool. That is unread
    // evidence, not a negative observation of the event.
    if wait_observer_unread(md)
        || md.get(PROOF_ATTEMPT).and_then(|a| a.get("waits_on_exit")) == Some(&Value::Null)
        || (w.seen.is_some()
            && md
                .get(PROOF_ATTEMPT)
                .is_some_and(|attempt| wait_observation(attempt) != WaitObservation::Unseen))
    {
        return None;
    }
    if starved(md).is_some() {
        return None;
    }
    Some(OwnedWait {
        owner,
        on: w.on,
        max_wait_hours: md
            .pointer(&format!("/{WAITS_ON}/{WAITS_ON_MAX_WAIT_HOURS}"))
            .and_then(Value::as_i64)
            .filter(|h| *h > 0),
    })
}

/// THE PROOF A LANDED CAR OWES (backlog b9005734, approved by David
/// 2026-09-24). The packet measured 21 cars standing at `proven`,
/// several five to seven days old, each waiting on a rare event nobody
/// here causes — a Stripe charge, a release David cuts, his destructive
/// prune, a red nightly crawl — and each holding its item open while the
/// shed read TROUBLED every hour of every day. A permanently red surface
/// is read like a silent one (CLAUDE.md §Diagnosis). So a car whose
/// wait is declared, observed and owned closes LANDED with its proof
/// owed, and the proof becomes an obligation keyed to the event it
/// waits on: when that event fires, the recorded probe runs again.
///
/// `proof_owed = "true"` is the one marker — a string, because a
/// protocol predicate compares strings (`job.metadata.proof_owed =
/// "true"`, the idiom every `abandoned` terminal uses) — and the
/// obligation reads it through [`owes_proof`]. Paying the proof turns it
/// to `"paid"` — kept, not removed, so the obligation's
/// `metadata_has=proof_owed` reader still finds a car that owed and was
/// paid — beside the proof itself under `owed_proof` (the forge door,
/// `boss prove --from-car --unattended`); `owes_proof` is false for it.
pub const PROOF_OWED: &str = "proof_owed";

/// The car metadata key an owed proof is paid under: the same record
/// `proven` carries when completed (`verified`, `proof`, `completed_at`)
/// plus `proven_by`, held on the car because its `proven` step was
/// skipped by the landing close and a closed packet's steps are history.
/// Here rather than in the CLI that writes it, because the server now
/// reserves it (backlog 82946cd9): one spelling for the writer and the
/// door that judges it (CLAUDE.md §9a).
pub const OWED_PROOF: &str = "owed_proof";

/// Does this car owe its proof? Only under the one marker.
pub fn owes_proof(md: &Value) -> bool {
    md.get(PROOF_OWED).and_then(Value::as_str) == Some("true")
}

/// The payment as a door READ it — both keys, `null` for an absent one —
/// as the precondition its write carries into the store (backlog
/// 87fe090d, review 7d92ec03 of car 5351793c). [`payment_moves`] judges
/// a write against the row the door read; the merge lands on the row as
/// it stands and the PUT saves a whole row, so a payment, a debt or an
/// erasure committed between the two would be undone by a write judged
/// harmless. The store refuses the write unless the row still reads so
/// under its lock ([`crate::step_metadata_write::metadata_still_reads`]).
/// Only these two keys: any other key written since changes nothing the
/// judgement rests on, and the conductor's markers land on a car all day.
pub fn payment_as_read(md: &Value) -> serde_json::Map<String, Value> {
    [OWED_PROOF, PROOF_OWED]
        .into_iter()
        .map(|k| (k.to_owned(), md.get(k).cloned().unwrap_or(Value::Null)))
        .collect()
}

/// WHO PAYS AN OWED PROOF (backlog 82946cd9, F3 of reviews 9daaa7a9 and
/// e7bf6b14 on car c8ecf66a). The payment is one metadata write — the
/// marker turned to `"paid"` beside the proof under [`OWED_PROOF`] — and
/// until this rule any caller with Update on job could make it through
/// either job door, indistinguishable from the forge's except by the
/// audit actor: the self-asserted `x-boss-user` id every machine-door
/// caller can type (the reason [`crate::field_writer`] never reads it).
/// A forged `"paid"` drops the car out of the obligation's reader, so
/// its probe is never run again, on a record that reads proven.
///
/// So the payment is reserved to the credentialed runner — the one
/// identity the server resolves rather than believes
/// ([`crate::runner_credential`]). These are the keys a write moves that
/// only it may move: [`OWED_PROOF`] in any way (set, changed or erased),
/// and [`PROOF_OWED`] in any way but one. Marking a debt — absent to
/// `"true"` — stays open to every writer, because the landing that owes
/// it is not the runner, and a forged mark admits nothing by itself:
/// the forge door re-reads the car and runs only a closed, landed,
/// undisproved one (review 9daaa7a9 F2). Erasing a debt, paying it,
/// un-paying it or re-owing a paid one is the runner's. `null` reads as
/// absent on both sides, so a merge-door `null` for a key the row does
/// not hold changes nothing.
pub fn payment_moves(old: &Value, new: &Value) -> Vec<&'static str> {
    let at = |md: &Value, key: &str| md.get(key).filter(|v| !v.is_null()).cloned();
    let mut moved = Vec::new();
    if at(old, OWED_PROOF) != at(new, OWED_PROOF) {
        moved.push(OWED_PROOF);
    }
    let (was, will) = (at(old, PROOF_OWED), at(new, PROOF_OWED));
    let marks_a_debt = was.is_none() && will.as_ref().and_then(Value::as_str) == Some("true");
    if was != will && !marks_a_debt {
        moved.push(PROOF_OWED);
    }
    moved
}

/// The verb that pays an owed proof, compiled in so the hosts it runs on
/// are READ rather than copied (CLAUDE.md §9a; backlog 82946cd9).
const RUN_CAR_PROBE_VERB: &str = include_str!("../../../../infra/ops/verbs/run-car-probe.json");

/// The hosts an owed proof may be paid from: the `hosts` the
/// run-car-probe verb declares — the only verb that runs a car's probe,
/// so the only place a payment can honestly come from. A verb file that
/// does not parse answers no host, which refuses every payment rather
/// than admitting any; the test beside [`payment_refusal`] pins it.
pub fn payment_hosts() -> Vec<String> {
    serde_json::from_str::<Value>(RUN_CAR_PROBE_VERB)
        .ok()
        .and_then(|verb| {
            verb.get("hosts")?.as_array().map(|hosts| {
                hosts
                    .iter()
                    .filter_map(|h| h.as_str().map(str::to_string))
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// Why `caller` may not move the payment keys, or `None` when it may:
/// the [`crate::runner_credential::PRINCIPAL`] credential, presented
/// from a host in [`payment_hosts`]. A car names no `host`, so the host
/// binding [`crate::field_writer::admits`] applies to a packet is not
/// asked of the car; the payment's own binding is asked instead.
///
/// HOST-BOUND (backlog 82946cd9, owed after car 5351793c): the runner
/// credential resolves on every runner host — forge and boss-gcp both
/// hold one since row E (2026-10-10) — and only the forge runs a car's
/// probe. Admitting any runner host let a host that never ran the probe
/// write the payment that says it passed. A credential that names no
/// host is refused the same way: it names nothing to judge.
pub fn payment_refusal(caller: Option<&crate::field_writer::CredentialedCaller>) -> Option<String> {
    let host = caller.and_then(|c| c.host.as_deref());
    if let Err(why) = crate::field_writer::admits(caller, crate::runner_credential::PRINCIPAL, host)
    {
        return Some(why);
    }
    let hosts = payment_hosts();
    if host.is_some_and(|h| hosts.iter().any(|p| p == h)) {
        return None;
    }
    let named = hosts
        .iter()
        .map(|h| format!("`{h}`"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "the presented credential is bound to host {}, and an owed proof is paid only from \
         a host the run-car-probe verb runs on ({}; infra/ops/verbs/run-car-probe.json \
         `hosts`)",
        host.map_or_else(|| "none".to_string(), |h| format!("`{h}`")),
        if named.is_empty() { "none" } else { &named },
    ))
}

/// The `spec_slug` of the step an owed proof is the proof of.
pub const PROVEN_STEP: &str = "proven";

/// What the packet read hands beside a step for an owed proof's
/// payment, or `None` (backlog 82946cd9, F4 of reviews 9daaa7a9 and
/// e7bf6b14). A landing close SKIPS `proven`, and the runner pays the
/// debt on the car, because a closed packet's steps are history — so a
/// reader of the step saw it skipped and nothing pointing at the proof
/// that paid it. The record under [`OWED_PROOF`], as stored, beside the
/// [`PROVEN_STEP`] step only; every write of it is the credentialed
/// runner's ([`payment_moves`]), so what is handed is the runner's.
pub fn owed_proof_beside(md: &Value, step_slug: Option<&str>) -> Option<Value> {
    (step_slug == Some(PROVEN_STEP))
        .then(|| md.get(OWED_PROOF).filter(|v| !v.is_null()).cloned())
        .flatten()
}

/// Inside `waits_on`: the event that settles the wait, declared so a
/// MACHINE can match it — `{"closes": <kind>, "title": <prefix>}`, the
/// close of a packet of that kind, optionally narrowed by the start of
/// its title (an `ops-request`'s title leads with its verb). `on` is
/// prose for a reader and `seen` a probe for the forge; neither can key
/// a dispatcher firing, which is what an owed proof needs (b9005734).
/// The close marker carries `kind` and `title` on all three emit sites,
/// so this is matched against the event itself, never a re-fetch.
pub const WAITS_ON_EVENT: &str = "event";

/// A declared wait's event, read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitEvent {
    /// The kind of packet whose close is the event.
    pub closes: String,
    /// Optional: the closing packet's title must start with this.
    pub title: Option<String>,
}

impl WaitEvent {
    /// Is this `jobs.job.closed` marker the declared event? The kind
    /// must match, and a declared prefix must lead the closing title —
    /// a marker with no title satisfies no prefix.
    pub fn fired_by(&self, closed: &Value) -> bool {
        let title = closed.get("title").and_then(Value::as_str);
        closed.get("kind").and_then(Value::as_str) == Some(self.closes.as_str())
            && self
                .title
                .as_deref()
                .is_none_or(|p| title.is_some_and(|t| t.starts_with(p)))
    }
}

/// A declared wait's event in the one shape [`wait_event`] reads — what
/// `boss car waits-on --event-closes` and a park file's
/// `[waits_on.event]` write (backlog 9e9db2ea). The title prefix is
/// omitted, never nulled, when none is given.
pub fn waits_on_event_value(closes: &str, title: Option<&str>) -> Value {
    let mut e = json!({"closes": closes});
    if let Some(t) = title {
        e["title"] = json!(t);
    }
    e
}

/// The event a car's `waits_on` declares, or `None` — for no wait, a
/// wait naming nothing ([`waits_on`] refuses a blank `on`), or an event
/// whose `closes` is blank, so an empty object can never key an
/// obligation to every close there is.
pub fn wait_event(md: &Value) -> Option<WaitEvent> {
    waits_on(md)?;
    let e = md.get(WAITS_ON)?.get(WAITS_ON_EVENT)?;
    Some(WaitEvent {
        closes: non_blank(e.get("closes"))?,
        title: non_blank(e.get("title")),
    })
}

/// THE ITEM A CAR ANSWERS, AND AUTHORISES THE CLOSE OF.
///
/// The declared one-to-one job edge (`('ship-a-change', 'backlog_item',
/// 'job_id')`) the arrival rule `complete-feedback-branch-on-car-merged`
/// follows to complete the item's `build` step when the change merges —
/// so writing it is what closes the item, and the id is ref-checked at
/// the write.
///
/// ONE DEFINITION, LATE (backlog 973d353f). Its two siblings below were
/// `pub const` from the day each was written, while the commonest of the
/// three — 187 of 200 recent cars carried it, against 6 and 7 — stayed a
/// bare string in fourteen files, several of them lines that name
/// [`PARTIAL_ITEM`] by constant and this one by literal. It had not
/// drifted; the asymmetry was the hazard on its own, because a reader
/// who finds two of three as constants concludes the pattern is
/// constants and never greps for the third (CLAUDE.md §9a: a pin is a
/// holding action, the collapse is the destination).
pub const BACKLOG_ITEM: &str = "backlog_item";

/// THE ITEM A CAR NAMES WITHOUT AUTHORISING ITS CLOSE.
///
/// `backlog_item` is a declared one-to-one job edge, and the arrival
/// rule `complete-feedback-branch-on-car-merged` follows exactly that
/// key to route the item's triage and complete its `build` — which
/// closes it. So a car that is ONE PIECE of a several-piece item must
/// not use it: on 2026-09-10 item cf0f5e2d was three pieces and held
/// car f73828a9 was piece (1) only, with piece (3) waiting on an
/// operator's explicit yes; linking it would have closed an item with
/// work outstanding. This key records the same provenance under a name
/// NO rule reads, which is what makes it inert at arrival.
pub const PARTIAL_ITEM: &str = "partial_item";

/// EVERY OTHER ITEM A CAR ANSWERS, AND AUTHORISES THE CLOSE OF.
///
/// A `job_id_list` edge beside [`BACKLOG_ITEM`], followed on merge by
/// the same arrival rule (its `also_link`), so each listed item is
/// routed and built exactly as the primary is. `backlog_item` holds ONE
/// id, and on 2026-09-23 two landed items stayed open that way and were
/// dispatched to builders who rediscovered the landing: 5994de6d (its
/// fix rode on cars filed under three other items) and cab50f4c (car
/// c842f18b named only 3ec04168). Backlog a994f533.
pub const ALSO_ANSWERS: &str = "also_answers";
/// Why this car names no item at all — the answer a deliberately
/// item-less car gives (`--park-no-item`). Item-less cars legitimately
/// exist (a fix asked for in conversation, a defect found while
/// building something else); the reason is what lets a later reader
/// tell one from a car whose builder simply forgot, which is the
/// omission e1325456 measured thirteen times in nineteen.
pub const NO_ITEM_REASON: &str = "no_item_reason";

/// THE ORDERING EDGE A CAR DECLARES: the car this one must land BEHIND.
///
/// A declared job edge (`('ship-a-change', 'boards_after', 'job_id')`,
/// migration 20260911-a-car-declares-what-it-boards-after), so the id is
/// ref-checked and prefix-normalised at the write the way `backlog_item`
/// is — a mistyped car is refused, not silently pointed at nothing.
///
/// WHY AN EDGE AND NOT A HOLD. Four cars were held by hand on
/// 2026-09-10 and every one of them was an ordering constraint:
/// `fix/the-reclaim-follows-the-build` had to be gated `--hold` and
/// parked by hand purely so it would board solo, and
/// `feat/a-deleted-manifest-leaves-no-object` spent a day waiting for a
/// human to notice both halves of a two-part sequence were satisfied.
/// An edge states the constraint once, in the one place a car's other
/// facts are already stated, and the dock enforces it every 60 seconds
/// without anyone watching. `--hold` stays: a declared edge and a human
/// brake are different tools (design doc 364f892e, backlog d3320278).
///
/// Two READERS, one judgement: `boss train board` filters its candidates
/// on it, and the dock region (`regions::dock_edges`) counts the cars it
/// holds — both through [`boards_after_outcome`] below, which names which
/// of four situations holds (backlog 4142d821). The key lives here
/// because the writers (`boss gate --park-after`, the auto-park handler)
/// and those readers must not keep two spellings of one fact (CLAUDE.md
/// §9a).
pub const BOARDS_AFTER: &str = "boards_after";

/// The gate-run key `boss gate --park-after` stamps, which the auto-park
/// handler reads and copies onto the car as [`BOARDS_AFTER`].
///
/// IN CORE, NOT SPELLED TWICE. The writer is the CLI and the reader is a
/// dispatcher handler, in two crates that cannot import each other; every
/// other `park_*` key is a string literal in both places, agreeing by
/// coincidence. One `pub const` cannot drift from itself (CLAUDE.md §9a),
/// and a typo on either side would be a car filed with an ordering
/// constraint the dock never sees — a hold the builder believes in and
/// nothing enforces.
pub const PARK_BOARDS_AFTER: &str = "park_boards_after";

/// THE REST OF THE `park_*` FAMILY, here for the reason the one above
/// already gave: the writer is the CLI and the reader is a dispatcher
/// handler, in two crates that cannot import each other, so a key
/// spelled in both places agrees only by coincidence.
///
/// Until 2026-09-21 that was the state of all ten (backlog ef9a602d).
/// Five had no constant at all; five more had one in `boss-cli` — a
/// const on the WRITE side and a bare literal on the READ side, which
/// is the same coincidence wearing a better name. `PARK_BOARDS_AFTER`
/// was the only key both sides took from one definition.
///
/// THE FAILURE IS SILENT, which is why it is worth ten constants. A
/// mistyped key on the write side stamps a field nobody reads; on the
/// read side it reads a field nobody stamped. Either way the car
/// parks, the gate is green, and the receipt is quietly short a piece
/// — and for `PARK_BACKLOG_ITEM` that piece is what attaches a landed
/// fix to the packet it answers, so losing it leaves an item open
/// after its fix is in production, which is the residue the startup
/// protocol sends the next session to re-derive by hand.
pub const PARK_SUMMARY: &str = "park_summary";
pub const PARK_EXCLUDES: &str = "park_excludes";
pub const PARK_TEST: &str = "park_test";
pub const PARK_VERIFIED: &str = "park_verified";
pub const PARK_BACKLOG_ITEM: &str = "park_backlog_item";
pub const PARK_PROBE: &str = "park_probe";
pub const PARK_EXPECT: &str = "park_expect";
pub const PARK_PROOF_EVENT: &str = "park_proof_event";
pub const PARK_NO_ITEM: &str = "park_no_item";
pub const PARK_PARTIAL_ITEM: &str = "park_partial_item";
/// `boss gate --park-waits-on` (backlog e9b164a1 piece 3): the car's
/// declared wait as the builder states it at park — an object carrying
/// only the [`WAITS_ON`] fields given. The auto-park handler MERGES it
/// onto the car's `waits_on` with [`merge_waits_on`], never replaces.
pub const PARK_WAITS_ON: &str = "park_waits_on";
/// `boss gate --park-also-answers` (backlog a994f533, the writer half):
/// every OTHER item the car answers, as a JSON list of full ids. The
/// auto-park handler copies it onto the car as [`ALSO_ANSWERS`] through
/// [`also_answers`]. Until this existed the edge had a reader and no
/// writer, so nothing a builder typed could set it.
pub const PARK_ALSO_ANSWERS: &str = "park_also_answers";

/// The `also_answers` edge a car carries, read off what the gate stamped
/// under [`PARK_ALSO_ANSWERS`]: each id once, in the order given, blanks
/// dropped. Anything but a list, or a list with no id in it, is omitted
/// rather than written empty — the same absent-never-nulled contract as
/// [`item_provenance`], so a re-gate that names none leaves a recorded
/// list alone.
pub fn also_answers(stamped: Option<&Value>) -> serde_json::Map<String, Value> {
    let ids = stamped
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .fold(Vec::<&str>::new(), |mut seen, id| {
                    if !seen.contains(&id) {
                        seen.push(id);
                    }
                    seen
                })
        })
        .unwrap_or_default();
    let mut m = serde_json::Map::new();
    if !ids.is_empty() {
        m.insert(ALSO_ANSWERS.to_string(), json!(ids));
    }
    m
}

/// The item provenance a car carries beyond the closing edge: the item
/// it is one piece of, or the reason it names none. Absent and blank
/// values are omitted (never nulled), so merging this into a car body
/// or a metadata PATCH adds what was stated and touches nothing else —
/// the same contract [`proof_intent`] has.
pub fn item_provenance(
    partial_item: Option<&str>,
    no_item_reason: Option<&str>,
) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    let mut put = |k: &str, v: Option<&str>| {
        if let Some(v) = v.filter(|s| !s.trim().is_empty()) {
            m.insert(k.to_string(), json!(v));
        }
    };
    put(PARTIAL_ITEM, partial_item);
    put(NO_ITEM_REASON, no_item_reason);
    m
}

/// The SET of tiers a change touched (`infra/platform/tiers.toml`,
/// design 01c3cc3f), sorted, stamped on the gate-run by `boss gate`
/// beside `delivery_channel` and carried onto the car — a sorted JSON
/// array of tier names, empty for a change no tier claims.
pub const SOFTWARE_TIERS: &str = "software_tiers";
/// The headline among [`SOFTWARE_TIERS`]: the lowest-ranked tier the
/// change touched (core wins over frontend), the way `delivery_channel`
/// is the heaviest of a mixed car's paths. Absent when the set is empty.
pub const SOFTWARE_TIER: &str = "software_tier";

/// The tier stamps a gate-run carries, copied VERBATIM for the car —
/// the same copy-don't-rebuild rule the receipt and the proof intent
/// live by (ba429e7f, 2026-09-19). A set that is not an array, or a
/// headline that is not a non-empty string, is omitted (never nulled:
/// the metadata door deletes a null key, and a re-gate that classified
/// nothing must not strip what the first park recorded).
pub fn tier_stamps(md: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    if let Some(set) = md.get(SOFTWARE_TIERS).filter(|v| v.is_array()) {
        m.insert(SOFTWARE_TIERS.to_string(), set.clone());
    }
    if let Some(head) = md
        .get(SOFTWARE_TIER)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        m.insert(SOFTWARE_TIER.to_string(), json!(head));
    }
    m
}

/// A step by its registry slug, falling back to its title. The same
/// lookup the conductor uses; one definition (CLAUDE.md 9a).
pub fn find_step<'a>(job: &'a Value, slug: &str, title: &str) -> Option<&'a Value> {
    let steps = job.get("steps").and_then(Value::as_array)?;
    steps
        .iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
        .or_else(|| {
            steps
                .iter()
                .find(|s| s.get("title").and_then(Value::as_str) == Some(title))
        })
}

/// Is this car still PARKED — gated, filed, and waiting at the dock —
/// rather than boarded (a train stamped it) or past review?
///
/// ONE PREDICATE, THREE READERS. The conductor counts the dock with it
/// (`parked_ready` adds boarding's own refinements: a `hold`, a
/// `train/` branch). `boss park` and the dispatcher's auto-park handler
/// ask it the question this file exists for: when a branch is gated
/// AGAIN, is there a car to refresh, or is a new one due? Measured
/// 2026-09-05 (backlog d052afad): every re-gate filed a twin, the dock
/// held 10 cars for 6 branches, and the first car of each pair sat
/// there with a receipt for a head that no longer existed — left behind
/// by every train as "gated, then changed" until closed by hand.
///
/// Status is deliberately not read here: callers list `status=open`,
/// and a fixture without a top-level status must still answer.
pub fn is_parked(car: &Value) -> bool {
    let md = car.get("metadata");
    let branch = md
        .and_then(|m| m.get("branch"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if branch.is_empty() || is_set(md.and_then(|m| m.get("train"))) {
        return false;
    }
    matches!(
        find_step(car, REVIEW_SLUG, REVIEW)
            .and_then(|s| s.get("status"))
            .and_then(Value::as_str),
        Some("ready" | "active")
    )
}

/// Is this car OPEN AT BUILD — filed when its build STARTED, and not yet
/// carrying a gate verdict?
///
/// THE THIRD STATE A LIVE CAR CAN BE IN, and it did not exist until
/// 2026-09-10. While a car was only ever filed at green, `is_parked` and
/// `is_boarded` between them covered every live car. A car opened at
/// build start (backlog be025b44) is neither: its `gate` step has not
/// reported, so `review` is not open, so the dock does not hold it — and
/// correctly, because a car with no receipt must never board. It is
/// nonetheless THE car for its branch, which is the whole point: the
/// branch has exactly one packet from its first minute, so "no parked
/// car" can never again be read as "no car" and file a twin (d052afad,
/// 02165b1d, 6790175e — three measured twins, all that shape).
///
/// The gate step must be PRESENT and open. Absent means this build of
/// boss cannot read the packet's shape, and an unreadable car is not one
/// to adopt — refuse, do not guess.
pub fn is_building(car: &Value) -> bool {
    let md = car.get("metadata");
    let branch = md
        .and_then(|m| m.get("branch"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if branch.is_empty() || !is_open(car) || is_set(md.and_then(|m| m.get("train"))) {
        return false;
    }
    matches!(
        find_step(car, GATE_SLUG, GATE)
            .and_then(|s| s.get("status"))
            .and_then(Value::as_str),
        Some("pending" | "ready" | "active")
    )
}

/// The car a builder already OPENED for `branch`, if one is still
/// building. `None` means no build is in flight for it under a packet.
pub fn building_car_for<'a>(cars: &'a [Value], branch: &str) -> Option<&'a Value> {
    cars.iter().find(|c| {
        c.pointer("/metadata/branch").and_then(Value::as_str) == Some(branch) && is_building(c)
    })
}

/// The car a re-gate of `branch` refreshes, if one is still parked.
/// `None` means file a new car — the branch has none, or its car has
/// boarded or reached a terminal and that history is spent.
pub fn parked_car_for<'a>(cars: &'a [Value], branch: &str) -> Option<&'a Value> {
    cars.iter().find(|c| {
        c.pointer("/metadata/branch").and_then(Value::as_str) == Some(branch) && is_parked(c)
    })
}

/// The car a park-flagged green of `branch` LANDS ON: the one parked at
/// the dock (the auto-park handler's `Refresh`), otherwise the one a
/// builder opened and is still building (its `Adopt`). `None` means the
/// green would file a car — or has nothing to land on, for a boarded one.
///
/// WHY IT IS SPELLED HERE (backlog 92a51856). [`unproven_park`] is asked
/// of the car a park lands on, and the handler asked it of this car
/// while `boss gate` asked it of none: a park-flagged re-gate of a car
/// that already carried its probe — CLAUDE.md §Doors' own repair — was
/// refused at launch unless it typed the probe again. One answer to
/// "which car" for both doors.
pub fn car_a_park_lands_on<'a>(cars: &'a [Value], branch: &str) -> Option<&'a Value> {
    parked_car_for(cars, branch).or_else(|| building_car_for(cars, branch))
}

/// Is this car ABOARD a train — an open car the conductor stamped with
/// `metadata.train` and has not released?
///
/// The other half of `is_parked`: between them they cover every live
/// car. Boarding stamps the train rather than completing the review
/// step, and a cancelled train clears the stamp, so the stamp is the
/// whole test — plus "not closed", because a closed car's train stamp
/// is history (that is what `is_landed` reads).
pub fn is_boarded(car: &Value) -> bool {
    let md = car.get("metadata");
    let branch = md
        .and_then(|m| m.get("branch"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    !branch.is_empty() && is_open(car) && is_set(md.and_then(|m| m.get("train")))
}

/// The live car this branch already has — aboard a train if one is,
/// otherwise the one parked at the dock, otherwise the one a builder
/// opened and is still building. `None` means the branch has no car: a
/// green may file one.
///
/// ONE PREDICATE, TWO CALLERS, ONE MEASURED BUG (backlog 02165b1d).
/// 2026-09-08 20:30:54: car d08a6418 boarded train #274. 20:31:20: a
/// duplicate gate-run went green and the auto-park handler filed car
/// ad54e95c for the SAME branch, which then sat on the loading dock
/// while the first rode — abandoned by hand. `parked_car_for` answered
/// `None` (correctly: the first car was no longer parked), and "no
/// parked car" was read as "no car". It is not. So both the handler and
/// `boss gate`'s launch guard ask THIS question — is there a live car
/// at all — from one definition (CLAUDE.md §9a).
/// THE BUILDING CAR IS A LIVE CAR TOO (be025b44). Since a builder opens
/// the car when the build starts, the commonest live state is no longer
/// "parked": it is "building", and a question that missed it would file
/// the fourth twin of this exact shape. Ordered as the failures were
/// measured — aboard, then at the dock, then building — so the earlier
/// answers cannot move.
pub fn open_car_for<'a>(cars: &'a [Value], branch: &str) -> Option<&'a Value> {
    let mine = |c: &&Value| c.pointer("/metadata/branch").and_then(Value::as_str) == Some(branch);
    cars.iter()
        .find(|c| mine(c) && is_boarded(c))
        .or_else(|| cars.iter().find(|c| mine(c) && is_open(c) && is_parked(c)))
        .or_else(|| cars.iter().find(|c| mine(c) && is_building(c)))
}

/// A car that already carried its branch to main: closed with
/// `outcome=merged`, or stamped `merged` by the conductor's landing (the
/// marker v3 ship-a-change gates its `merged` step on — the dispatcher
/// closes the Job from it, so a car can be marked before it is closed).
/// An abandoned or cancelled car is spent, not landed.
///
/// And a car whose landing main LOST is not landed either (backlog
/// f9256445): it keeps `merged` — the forge did merge it, and the record
/// keeps that — and carries the `unlanded` marker `boss car unland`
/// writes (`car_unland::UNLANDED_MARKER`). Read as landed, its
/// successor's re-gate would be skipped as landed content, and the work
/// could never ride again.
pub fn is_landed(car: &Value) -> bool {
    let md = car.get("metadata");
    if is_marked(car, crate::car_unland::UNLANDED_MARKER) {
        return false;
    }
    let closed_merged = car.get("status").and_then(Value::as_str) == Some("closed")
        && md.and_then(|m| m.get("outcome")).and_then(Value::as_str) == Some("merged");
    closed_merged || is_marked(car, "merged")
}

/// Does this car carry the marker `key` in its metadata? The conductor
/// writes a marker as the STRING "true"; a bool is accepted for the same
/// meaning, and an explicit false is neither. ONE reading for every
/// marker (backlog 82946cd9): the owed-proof door read `disproved` and
/// `abandoned` as the string only while [`is_landed`] read both forms,
/// so a bool-disproved owed car was run.
pub fn is_marked(car: &Value, key: &str) -> bool {
    match car.get("metadata").and_then(|m| m.get(key)) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

/// The car that already landed `branch`, if one has — the question both
/// `boss gate` and the auto-park handler ask BEFORE filing. Measured
/// 2026-09-08 (backlog 610537b2): a re-gate of a landed branch reused a
/// gate-run that still carried park intent, and on green the handler
/// filed a twin car for content already on main; it boarded, its train
/// went red on an empty diff, and train and twin were cleaned up by
/// hand. One test of "landed" here so the CLI and the dispatcher cannot
/// disagree about it.
pub fn landed_car_for<'a>(cars: &'a [Value], branch: &str) -> Option<&'a Value> {
    cars.iter().find(|c| {
        c.pointer("/metadata/branch").and_then(Value::as_str) == Some(branch) && is_landed(c)
    })
}

/// The job-metadata key of a car's count of consecutive CONFLICT
/// refusals — assembly's, written beside `skip_reason` (backlog
/// 94896e74), ended by boarding and by a re-park ([`regate_patch`]).
/// Spelled once here (design b35456ac) because four readers and
/// writers held it as a literal, and a dispatcher rule now reads it
/// too: `rerail-a-car-left-behind-on-a-conflict` files the repair at
/// exactly three, so a renamed key would silently retire the trigger.
pub const SKIPS: &str = "skips";

/// The job-metadata key of a car's count of consecutive departures the
/// dock's conductor held it through (backlog 2fccbfd6). The conductor
/// counts it; a re-park — this module's [`regate_patch`] — ends it.
pub const LEFT_BEHIND_TRAINS: &str = "left_behind_trains";

/// Beside it: the alarm filed for the current streak, so the streak files
/// no twin. It ends WITH the streak — every write that ends the count
/// nulls this too, or the next streak could never alarm.
pub const LEFT_BEHIND_ALARM: &str = "left_behind_alarm";

/// The metadata patch that supersedes a parked car's gate receipt.
///
/// A completed step is frozen, so a fresh receipt rides the JOB as
/// `regate_receipt` — the key boarding (`receipt_skip_reason`) and
/// `boss receipt` both read in preference to the gate step. VERBATIM,
/// like the gate step's copy: a rebuilt receipt once let a wrong head
/// through. `skip_reason` is present-and-null on purpose — the metadata
/// door deletes a null key, so the conductor's "left behind" reason
/// goes with the stale receipt, and `skips`, the count it headed, with
/// it. One builder for `boss park`, the
/// auto-park handler and `boss rerail`, so the write cannot drift.
pub fn regate_patch(receipt: &Receipt, note: &str, delivery_channel: Option<&str>) -> Value {
    let mut patch = json!({
        "regate_receipt": receipt.raw,
        "skip_reason": Value::Null,
        // And the count of consecutive refusals that reason headed
        // (backlog 7e941603): a re-gate is the repair of the conflict it
        // counted, so it ends it the way boarding does — null, which
        // `next_skip_count` reads as none. Left behind, car d89bafeb read
        // 13 skips with no reason through four conductor writes.
        SKIPS: Value::Null,
        "regate_note": note,
        // A re-parked car's dock streak is over (backlog 2fccbfd6): it has
        // a fresh receipt, so the holds it was counted through have ended.
        LEFT_BEHIND_TRAINS: Value::Null,
        LEFT_BEHIND_ALARM: Value::Null,
    });
    // Re-classify the channel from the re-gated diff and carry it, the
    // same field `car_body` stamps on a fresh car. Without this a
    // re-gate — the common path: every `--wait --park` of an existing
    // car, plus `boss park`/`boss rerail` — dropped `delivery_channel`,
    // so the yard and the channel mixes under-counted every branch that
    // was ever rebased or rerailed. Omitted (not nulled) when the diff
    // could not be classified: a null key is DELETED by the metadata
    // door, which would strip a channel the car already carried.
    if let Some(dc) = delivery_channel {
        patch["delivery_channel"] = json!(dc);
    }
    patch
}

/// The prose a re-gate carries onto a parked car, beside the receipt it
/// supersedes.
///
/// A car's account of itself — summary, excludes, test, verified — is
/// stamped on its scope/build/gate steps at the first park, and a
/// completed step is frozen. So until 2026-09-16 (c30e6276) a re-gate
/// wrote the fresh receipt, note and proof onto the job and left the
/// prose at the first build's words: a rebuilt car described what it
/// USED to do, to the yard, the train and the operator. The same rule
/// the receipt lives by applies — the frozen step stays as the first
/// head's record, and the current account rides the JOB under
/// `regate_*`, verbatim, where `regate_receipt` already is. Empty
/// prose is omitted, never nulled: the metadata door deletes a null
/// key, and a re-gate that restated nothing must not strip what an
/// earlier one carried.
pub fn regate_prose(
    summary: &str,
    excludes: &str,
    test: &str,
    verified: &str,
) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    for (key, text) in [
        ("regate_summary", summary),
        ("regate_excludes", excludes),
        ("regate_test", test),
        ("regate_verified", verified),
    ] {
        let text = text.trim();
        if !text.is_empty() {
            m.insert(key.to_string(), json!(text));
        }
    }
    m
}

/// Is this packet still open? Callers list `status=open`, so the field
/// is usually redundant — and a fixture without one must still answer —
/// but a list that also holds finished cars (the handler pages both)
/// must not read one of those as live.
///
/// BOTH terminal statuses, not just `closed`. The jobs API has two
/// (`triage_on_park` and `sweep_settled` both already say
/// `closed | cancelled`), and a cancelled car is as done as a closed
/// one: it cannot be boarded, parked onto, rerailed, or take a proof.
/// Reading one as live is the same defect this predicate exists to
/// prevent, one status over.
///
/// Public because `boss prove` asks it too: its read is deliberately
/// `kind=ship-a-change` with NO status filter (`--recheck` re-runs a
/// proof on a closed car), so it is the other caller holding a mixed
/// list. One definition for "is this car live", not a fourth copy of
/// the terminal-status test (CLAUDE.md §9a).
pub fn is_open(car: &Value) -> bool {
    !matches!(
        car.get("status").and_then(Value::as_str),
        Some("closed" | "cancelled")
    )
}

/// A metadata stamp that is present: the conductor writes `train` as
/// an id and clears it to `null` on release; an empty string reads as
/// cleared too.
fn is_set(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// The kinds of linked packet a park may ROUTE — the same two the
/// merge-time route lists (`complete-feedback-branch-on-car-merged`
/// v4, `route = [...]`), so the park and the merge agree about whose
/// triage a car may complete.
///
/// `user-feedback` joined on 2026-09-14 (backlog a29c3687). Until then
/// a feedback packet's triage was "the filer's own routing decision"
/// and the park left it alone — but a car PARKED AGAINST the packet is
/// that decision already made, by whoever built and linked the car,
/// and the v4 merge route closes it on the same reasoning. Leaving the
/// hours between park and merge un-routed put David's 9827c699 on the
/// feedback board as nobody's decision for seventy minutes while its
/// car sat on the dock and then rode a train. Its triage vocabulary
/// (`reproduce|design|build|duplicate|needs-info|decline`,
/// infra/platform/workflows/user-feedback.toml) admits `build`; the
/// `build` step then opens by `ready_when` and the merge route
/// completes it. A feedback packet with no car parked against it never
/// reaches this function, so its triage stays a person's.
pub const TRIAGEABLE_KINDS: &[&str] = &["backlog-item", "user-feedback"];

/// The routing step on that kind. Its `spec_slug` is the lookup;
/// `find_step`'s title fallback is given the same string because this
/// step has no separate title a car author could rely on.
pub const TRIAGE_SLUG: &str = "triage";

/// The disposition a parked car states: this car is the item's build.
pub const DISPOSITION_BUILD: &str = "build";

/// PURE: the triage write parking this car owes the item it links — or
/// `None` when there is nothing for a park to state. The HTTP stays with
/// each caller (`boss park`, `boss car open` and the dispatcher's
/// auto-park handler); the DECISION lives here once.
///
/// THE SAME TWO WRITES AS EVERY OTHER CAR WRITE — a [`StepWrite`],
/// addressed through the ITEM's id: the route through the step merge
/// door, then a status-only PUT (backlog e39a9d2a, Stage 1). This was
/// one PUT of `{status, metadata}` whose metadata was the step's as the
/// park had READ it plus the two route keys; the PUT replaces metadata
/// wholesale, so a key written between that read and the PUT was dropped
/// by omission. The merge body holds the route alone, because the merge
/// keeps what the step already carries.
///
/// WHY A PARK DECIDES THE ROUTE. `--park-backlog-item <id>` says "this
/// car is that item's build". But the item's `build` step only OPENS
/// once its triage records `disposition = build`, so a car parked
/// against an un-triaged item links to a step nothing can advance: the
/// change gets built, gated, landed and proven while the item still
/// reads as undecided work, and the arrival rule finds nothing to
/// complete. Measured twice within an hour on 2026-09-09 (items
/// 5942f205 and af28e250; backlog ca76d8f9) — once walked through by
/// hand afterwards, once triaged by the builder so the arrival rule
/// would have something to complete. A car cannot be the build of
/// something nobody decided to build, so the flag that names it states
/// the decision at park time rather than leaving residue: an open
/// packet that looks like unfinished work when the work is live. That
/// residue is not free — past 100 open `ship-a-change` jobs a
/// parked-ready car falls off page 1 and never boards (874ea0ae).
///
/// IDEMPOTENT, AND NEVER DESTRUCTIVE. `Some` comes back only while the
/// routing step is still OPEN (`ready`/`active` — the same "open" the
/// arrival rule's route reads) AND carries no disposition. A triage a
/// person already completed, an item a person routed to `verify` /
/// `design` / `stale` / `decline` (or feedback its filer sent to
/// `reproduce` / `needs-info`), a closed or cancelled packet, a packet
/// with no routing step, and a kind outside `TRIAGEABLE_KINDS` all
/// answer `None` — so a re-gate, a refresh or a redelivery writes
/// nothing, and no human's disposition is ever overwritten.
pub fn triage_on_park(item: &Value, car_id: &str, branch: &str) -> Option<StepWrite> {
    let kind = item.get("kind").and_then(Value::as_str)?;
    if !TRIAGEABLE_KINDS.contains(&kind) {
        return None;
    }
    if matches!(
        item.get("status").and_then(Value::as_str),
        Some("closed" | "cancelled")
    ) {
        return None;
    }
    let step = find_step(item, TRIAGE_SLUG, TRIAGE_SLUG)?;
    if !matches!(
        step.get("status").and_then(Value::as_str),
        Some("ready" | "active")
    ) {
        return None;
    }
    // Belt to the status guard's braces: a disposition already written
    // is a decision already made, whatever the step's status says.
    if step
        .get("metadata")
        .and_then(|m| m.get("disposition"))
        .is_some()
    {
        return None;
    }
    let step_id = step.get("id").and_then(Value::as_str)?;
    Some(StepWrite::completing(
        step_id,
        TRIAGE_SLUG,
        json!({
            "disposition": DISPOSITION_BUILD,
            "evidence": park_triage_evidence(car_id, branch),
        }),
    ))
}

/// The `evidence` the routing step records at done, naming WHAT made
/// the decision — a reader of the packet should not have to go find
/// out why its route says `build`. Shaped as the merge route's own
/// sentence is (`shipped and proven: {branch} — {title} (car {car})`,
/// complete-feedback-branch-on-car-merged v4): the moment differs, the
/// form does not, so the two reads on one packet's history read as
/// one story. `backlog-item`'s triage REQUIRES this key at done;
/// `user-feedback`'s declares only `finding`, and an extra key is
/// carried, not refused — the same key the merge route writes there.
fn park_triage_evidence(car_id: &str, branch: &str) -> String {
    format!(
        "routed at park: {branch} is this packet's build (car {car_id}). \
         --park-backlog-item names the car as the build, so the route is stated when \
         the car is filed rather than left un-triaged until the car merges \
         (backlog ca76d8f9, a29c3687)."
    )
}

// ---------------------------------------------------------------------------
// THE DECLARED ORDERING EDGE, JUDGED — one answer for the conductor and the dock
//
// Moved here from `boss-cli/src/train/boarding.rs` (backlog 4142d821, design
// cf820810 Q7). Until then only the conductor asked whether a parked car's
// `boards_after` predecessor had landed, so the conductor refused the car
// every 60 seconds while the dock region counted it as boardable and said
// "the boarding depth is met, a train is due" — the sentence it says two
// minutes after a healthy departure. Two readers of one edge must not keep
// two answers to "can this car board" (CLAUDE.md §9a), so the judgement
// lives in core beside the key it reads, and both take it from here.
//
// The words below are the conductor's own and are unchanged by the move:
// four situations, told apart at a glance, because an operator reading them
// is deciding whether the pipeline is stuck (d3320278):
//
//   still in flight  — nobody does anything; it departs on its own
//   landed           — satisfied; the car boards (no refusal at all)
//   abandoned        — a human must break the edge; it will never clear
//   no such Job      — a human must fix the reference
//
// AND IT MUST NEVER FREEZE A LANDING: an edge that cannot be READ boards the
// car and says why (`BoardUnjudged`). The edge exists to stop a known
// collision, not to become a new way for the pipeline to stop.
// ---------------------------------------------------------------------------

/// The structured marker a boarding-edge hold leaves on its `left_behind`
/// entry, so the window's own refusal line is composed from DATA and not
/// from sniffing the reason string back apart.
pub const EDGE_HOLD: &str = "edge_hold";
/// The predecessor is still in flight — self-clearing, no action.
pub const EDGE_HOLD_WAITING: &str = "waiting";
/// The edge can never be satisfied as declared — a person must act.
pub const EDGE_HOLD_NEEDS_HUMAN: &str = "needs_human";

/// How a hold names a predecessor outside the reader's policy scope
/// ([`Predecessor::Unscoped`]) — the whole of what it says about it.
pub const UNSCOPED_PREDECESSOR: &str = "a predecessor outside this caller's policy scope";

/// What a reader managed to learn about a car's declared predecessor.
/// `Unreadable` is a first-class answer, not an error: "I could not ask"
/// must be distinguishable from "it is not there".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Predecessor {
    /// The Job came back — as the jobs API serves it, with its `steps` —
    /// judged by this file's own predicates so no reader can disagree with
    /// the rest of the system about what "landed" means.
    Found(Value),
    /// The Job came back, but outside the READER's policy scope (backlog
    /// 0964ba80) — a narrowed caller's own car declaring another owner's
    /// car as its predecessor. It is judged by the same predicates, so
    /// the dock still says the car cannot board, but the refusal names
    /// nothing about it: no branch, no title, no state. The conductor
    /// reads at full scope and never builds one.
    Unscoped(Value),
    /// The jobs API answered that there is no such Job.
    Absent,
    /// The read itself failed — a blip, an outage, a malformed body.
    Unreadable(String),
}

/// Why a car may not board on its declared edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeHold {
    /// The reason, journal and Job chip alike — ONE string, as every
    /// other skip reason is.
    pub reason: String,
    /// `EDGE_HOLD_WAITING` or `EDGE_HOLD_NEEDS_HUMAN`.
    pub kind: &'static str,
    /// The predecessor as a reader names it — its branch and id8 where
    /// the packet came back, the id8 alone where it did not. Structured
    /// so the dock can say "waiting behind X" without parsing `reason`.
    pub behind: String,
}

/// What boarding should do about a car's declared edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeOutcome {
    /// Board it: the edge is satisfied, or there is none.
    Board,
    /// Board it, and SAY why the edge could not be judged. Fail-open by
    /// design — see the section comment.
    BoardUnjudged(String),
    /// Leave it behind, with the reason named on it.
    Hold(EdgeHold),
}

/// The predecessor a car's METADATA declares, if it declares one. A blank
/// value is no declaration — the metadata door deletes a null key but a
/// `""` is a real stored value, and `jobs_clear_waiting` shows `""` is how
/// an edge gets cleared in practice.
pub fn boards_after_of(md: &Value) -> Option<String> {
    md.get(BOARDS_AFTER)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The predecessor a car (a whole packet) declared, if it declared one.
pub fn declared_predecessor(car: &Value) -> Option<String> {
    car.get("metadata").and_then(boards_after_of)
}

/// The first eight characters of an id — the spelling every journal,
/// report and surface prints.
fn id8(id: &str) -> String {
    id.chars().take(8).collect()
}

/// How to name a predecessor in a refusal an operator reads: its BRANCH
/// where we have it, never a bare id (MEMORY: refer by protocol + title).
/// The id8 rides along so the packet is still findable.
fn predecessor_name(declared: &str, pred: Option<&Value>) -> String {
    let branch = pred
        .and_then(|p| p.pointer("/metadata/branch"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if branch.is_empty() {
        format!("car {}", id8(declared))
    } else {
        format!("{branch} (car {})", id8(declared))
    }
}

/// Where a live predecessor actually is, so "still in flight" names a
/// place rather than asserting a mood. The three states are this file's
/// (`is_boarded` / `is_parked` / `is_building`), in the order a car
/// passes through them backwards.
fn in_flight_at(pred: &Value) -> String {
    if is_boarded(pred) {
        let train = pred
            .pointer("/metadata/train")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if train.is_empty() {
            "aboard a train".to_string()
        } else {
            format!("aboard train {}", id8(train))
        }
    } else if is_parked(pred) {
        "parked at the dock".to_string()
    } else if is_building(pred) {
        "still building".to_string()
    } else {
        "open".to_string()
    }
}

/// How a spent predecessor ended, read off the packet rather than
/// guessed, so the refusal says what the record says.
fn spent_as(pred: &Value) -> String {
    let status = pred
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("not open");
    match pred.pointer("/metadata/outcome").and_then(Value::as_str) {
        Some(o) if !o.is_empty() => format!("{status}, outcome '{o}'"),
        _ => format!("{status}, no landing recorded"),
    }
}

/// PURE: what boarding does about one car's declared edge.
///
/// The four situations, and the exact words each gets. They are written
/// to be told apart at a glance by an operator scanning the journal:
/// "STILL IN FLIGHT" carries "no action needed", and both unsatisfiable
/// cases carry "a human must". That distinction is the feature — not the
/// hold (David on d3320278: "the refusal's wording matters as much as its
/// existence").
pub fn boards_after_outcome(declared: &str, pred: &Predecessor) -> EdgeOutcome {
    match pred {
        // FAIL-OPEN, LOUDLY. A car that would have boarded yesterday must
        // not be held because the system of record blipped while the
        // conductor asked about its edge.
        Predecessor::Unreadable(cause) => EdgeOutcome::BoardUnjudged(format!(
            "boards after car {}, and that packet could not be read ({cause}) — boarding \
             anyway: an unreadable edge is not evidence of a collision, and holding the \
             dock on a read failure would stop every train",
            id8(declared)
        )),
        Predecessor::Absent => EdgeOutcome::Hold(EdgeHold {
            reason: format!(
                "boards after car {}, which DOES NOT EXIST — a human must fix \
                 metadata.{} on this car (the edge is ref-checked at the write, so this \
                 id was stored before the edge was declared, or with ref-checking off)",
                id8(declared),
                BOARDS_AFTER
            ),
            kind: EDGE_HOLD_NEEDS_HUMAN,
            behind: predecessor_name(declared, None),
        }),
        Predecessor::Found(p) | Predecessor::Unscoped(p) if is_landed(p) => EdgeOutcome::Board,
        // OUTSIDE THE READER'S SCOPE (backlog 0964ba80): that it holds
        // the car, and nothing else. In flight or abandoned is the
        // predecessor's state, so both are one hold of one kind here —
        // the waiting kind, which claims no person must act — and the
        // full scope, which the conductor and the operator read at, sees
        // which.
        Predecessor::Unscoped(_) => EdgeOutcome::Hold(EdgeHold {
            reason: format!(
                "boards after {UNSCOPED_PREDECESSOR}, which holds it — what that car is and \
                 where it stands are not read for this caller's scope"
            ),
            kind: EDGE_HOLD_WAITING,
            behind: UNSCOPED_PREDECESSOR.to_string(),
        }),
        Predecessor::Found(p) if is_open(p) => EdgeOutcome::Hold(EdgeHold {
            reason: format!(
                "boards after {}, which is STILL IN FLIGHT ({}) — no action needed; this \
                 car boards on a later window once that one lands",
                predecessor_name(declared, Some(p)),
                in_flight_at(p)
            ),
            kind: EDGE_HOLD_WAITING,
            behind: predecessor_name(declared, Some(p)),
        }),
        Predecessor::Found(p) => EdgeOutcome::Hold(EdgeHold {
            reason: format!(
                "boards after {}, which was ABANDONED ({}) — the edge can never be \
                 satisfied; a human must clear metadata.{} on this car, or abandon it too",
                predecessor_name(declared, Some(p)),
                spent_as(p),
                BOARDS_AFTER
            ),
            kind: EDGE_HOLD_NEEDS_HUMAN,
            behind: predecessor_name(declared, Some(p)),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_observation_keeps_error_and_legacy_records_unread() {
        use WaitObservation::{Seen, Unread, Unseen};
        for (attempt, expected) in [
            (json!({"waits_on_exit": 0}), Seen),
            (
                json!({"waits_on_exit": 1, "waits_on_observation": "unseen"}),
                Unseen,
            ),
            (json!({"waits_on_exit": 1}), Unread),
            (
                json!({"waits_on_exit": 2, "waits_on_observation": "unseen"}),
                Unread,
            ),
            (json!({"waits_on_exit": -1}), Unread),
            (json!({"waits_on_exit": 256}), Unread),
            (
                json!({"waits_on_exit": 1.0, "waits_on_observation": "unseen"}),
                Unread,
            ),
            (
                json!({"waits_on_exit": true, "waits_on_observation": "unseen"}),
                Unread,
            ),
            (
                json!({"waits_on_exit": "1", "waits_on_observation": "unseen"}),
                Unread,
            ),
            (
                json!({"waits_on_exit": 1, "waits_on_observation": {"unseen": true}}),
                Unread,
            ),
            (
                json!({"waits_on_exit": 1, "waits_on_observation": "unseen", "waits_on_missing_tools": ["observer"]}),
                Unread,
            ),
            (
                json!({"waits_on_exit": 0, "waits_on_refusal": {"rule": "refused"}}),
                Unread,
            ),
            (
                json!({"waits_on_exit": 1, "waits_on_observation": "unseen", "waits_on_error": "could not execute"}),
                Unread,
            ),
        ] {
            assert_eq!(wait_observation(&attempt), expected, "{attempt}");
        }
    }

    #[test]
    fn an_environment_refusal_preserves_retry_accounting_but_is_not_an_observed_wait() {
        let refusal = json!({
            "exit": null, "not_yet": true, "environment": true,
            "probe": "echo claim:ok", "why": "host refused the execution environment"
        });
        assert!(attempt_said_not_yet(&refusal));
        let metadata = json!({"proof_probe": "echo claim:ok", "proof_attempt": refusal});
        assert_eq!(
            crate::regions::shed_place(&metadata),
            crate::regions::ShedPlace::ProbeUnread
        );
        assert!(!attempt_said_not_yet(
            &json!({"exit": 1, "not_yet": true, "environment": true})
        ));
        assert!(!attempt_said_not_yet(
            &json!({"exit": null, "not_yet": true})
        ));
        assert_eq!(
            crate::regions::shed_place(&json!({
                "proof_probe": "echo claim:ok",
                "proof_attempt": {"exit": 1, "environment": true, "why": "actual failure"}
            })),
            crate::regions::ShedPlace::ProbePending {
                last: Some("actual failure".into())
            }
        );
    }

    const HEAD: &str = "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8";
    const GREEN: &str = r#"{"verdict": "green", "head": "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8", "mode": "full", "fails": []}"#;

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().into()
    }

    /// A HOLD WRITTEN AFTER A RELEASE DELETES THE HEAD THAT RELEASE LIFTED
    /// (backlog 6c463c66). The review step of a released car keeps
    /// `release`, `hold_sha` and `agent_run`. The red-train hold and a
    /// hand `boss hold` used to write `hold` alone, so the old `hold_sha`
    /// read as the head the new hold was judged at.
    #[test]
    fn a_hold_after_a_release_deletes_the_head_the_release_lifted() {
        let h1 = "1111111111111111111111111111111111111111";
        let released = json!({ HOLD_SHA: h1, "agent_run": "run-r",
                               RELEASE: {"sha": h1, "review": "run-r"} });
        let body = hold_body(
            &released,
            " held by the conductor: train t1 was judged red ",
        );
        assert_eq!(
            body,
            json!({ "hold": "held by the conductor: train t1 was judged red", HOLD_SHA: null }),
            "the hold, and the lifted head deleted at the merge door; the release and the \
             reviewer run stay, as the record of what was released"
        );

        // KEPT: a live review hold keeps its head. A hand hold over the
        // conductor's re-hold at H2 is still that review hold.
        let live = json!({ "hold": "conductor re-judge at 222222222222", HOLD_SHA: "22",
                           RELEASE: {"sha": h1} });
        assert_eq!(hold_body(&live, "x"), json!({ "hold": "x" }));
        // KEPT: a review hold cleared bare, with no release: the conductor
        // holds that car again for its review, and needs the head to.
        let bare = json!({ HOLD_SHA: h1, "hold": null });
        assert_eq!(hold_body(&bare, "x"), json!({ "hold": "x" }));
        // A step with no head to delete gets the one key, as before.
        let none = json!({ RELEASE: {"sha": h1} });
        assert_eq!(hold_body(&none, "x"), json!({ "hold": "x" }));
        assert_eq!(hold_body(&Value::Null, "x"), json!({ "hold": "x" }));
    }

    /// A REFRESH CARRIES A LIVE REVIEW HOLD TO ITS NEW HEAD, AND ONLY THAT
    /// (backlog 6b12a70f). The head alone is written; nothing else moves.
    #[test]
    fn a_live_review_hold_follows_the_head_a_refresh_moved_the_car_to() {
        let held = json!({ "hold": "conductor re-judge at 29943365", HOLD_SHA: "29943365",
                           RELEASE: {"sha": "2b1591c1"} });
        assert_eq!(
            hold_follows_head(&held, " df139b70 "),
            Some(json!({ HOLD_SHA: "df139b70" }))
        );
        // Already at that head, or no head to move to: nothing.
        assert_eq!(hold_follows_head(&held, "29943365"), None);
        assert_eq!(hold_follows_head(&held, " "), None);
        // A hold bound to no head is not a review's: none is bound.
        for unbound in [
            json!({ "hold": "David only" }),
            json!({ "hold": "x", HOLD_SHA: null }),
            json!({ "hold": "x", HOLD_SHA: "" }),
        ] {
            assert_eq!(hold_follows_head(&unbound, "df139b70"), None, "{unbound}");
        }
        // No live hold — released, or cleared bare — carries nothing: the
        // conductor re-judges those at the head it boards.
        for clear in [
            json!({ HOLD_SHA: "29943365", "hold": null, RELEASE: {"sha": "29943365"} }),
            json!({ HOLD_SHA: "29943365", "hold": false }),
            Value::Null,
        ] {
            assert_eq!(hold_follows_head(&clear, "df139b70"), None, "{clear}");
        }
    }

    /// The judgement a carried hold rests on is the raw hold and head as
    /// read, nothing else, and it reads back true on the row it came from
    /// (backlog cd1f68c6).
    #[test]
    fn a_carried_hold_is_judged_on_the_hold_and_head_it_read() {
        let held = json!({ "hold": "conductor re-judge at 29943365", HOLD_SHA: "29943365",
                           RELEASE: {"sha": "2b1591c1"}, "agent_run": "7cb2438c" });
        let judged = hold_as_judged(&held);
        assert_eq!(
            Value::Object(judged.clone()),
            json!({ "hold": "conductor re-judge at 29943365", HOLD_SHA: "29943365" })
        );
        assert!(crate::step_metadata_write::metadata_still_reads(
            &held, &judged
        ));
        let released = json!({ HOLD_SHA: "29943365", RELEASE: {"sha": "29943365"} });
        assert!(!crate::step_metadata_write::metadata_still_reads(
            &released, &judged
        ));
        assert_eq!(
            Value::Object(hold_as_judged(&Value::Null)),
            json!({ "hold": null, HOLD_SHA: null })
        );
    }

    /// A STRIKE RELEASE LIFTS THE HOLD ITS STRIKE PLACED, AND ONLY THAT
    /// (backlog 79129752). Car 775feba5's review step, its hold verbatim.
    #[test]
    fn a_strike_release_lifts_only_the_red_train_hold_for_a_train_it_cleared() {
        const TRAIN: &str = "9b5c1889-f729-4fda-8546-8802806ec539";
        let hold = format!(
            "{} (test) as the only car aboard — first failing line: test: \
             every_converge_tick_observes_the_site::the_page_phase_stops_at_the_observation_\
             deadline_and_says_so - panicked at ... — rebuild it on current main, re-gate, then \
             `boss release` it (a2d4d842, b4a95f47)",
            judged_red_hold_head("9b5c1889")
        );
        assert!(hold.starts_with("held by the conductor: train 9b5c1889 was judged red (test)"));
        assert_eq!(judged_red_hold_train(&hold), Some("9b5c1889"));
        let md = json!({ "hold": hold, "authority_role": "platform-admin" });
        let cleared = vec![TRAIN.to_string()];
        assert_eq!(
            hold_lifted_by_strike_release(&md, &cleared, 0),
            Some(json!({ "hold": null }))
        );
        // A null head is no head.
        let mut unbound = md.clone();
        unbound[HOLD_SHA] = Value::Null;
        assert!(hold_lifted_by_strike_release(&unbound, &cleared, 0).is_some());

        // KEPT: a strike still standing, or a train this look did not clear.
        assert_eq!(hold_lifted_by_strike_release(&md, &cleared, 1), None);
        assert_eq!(hold_lifted_by_strike_release(&md, &[], 0), None);
        let other = vec!["0badc0de-0000-4000-8000-000000000000".to_string()];
        assert_eq!(hold_lifted_by_strike_release(&md, &other, 0), None);
        // KEPT: the same text bound to a head, or reserved for a person.
        let mut bound = md.clone();
        bound[HOLD_SHA] = json!("e76ef0f9");
        assert_eq!(hold_lifted_by_strike_release(&bound, &cleared, 0), None);
        let mut reserved = md.clone();
        reserved[RELEASER] = json!("emp-david");
        assert_eq!(hold_lifted_by_strike_release(&reserved, &cleared, 0), None);
        // KEPT: any other hold, and no hold at all.
        for kept in [
            json!({ "hold": "held for the restart" }),
            json!({ "hold": "conductor re-judge at 29943365", HOLD_SHA: "29943365" }),
            json!({ "hold": "the conductor said train 9b5c1889 was judged red" }),
            json!({ "hold": null }),
            Value::Null,
        ] {
            assert_eq!(
                hold_lifted_by_strike_release(&kept, &cleared, 0),
                None,
                "{kept}"
            );
        }
        // The recognizer reads only the head's own shape.
        assert_eq!(
            judged_red_hold_train("held by the conductor: train  was judged red"),
            None
        );
        assert_eq!(
            judged_red_hold_train("held by the conductor: train a b was judged red"),
            None
        );
    }

    #[test]
    fn malformed_review_gate_envelope_does_not_delete_a_still_held_review_head() {
        for poison in [json!({}), json!(null)] {
            let plain = json!({"hold":"David only", HOLD_SHA:"head", RELEASE:{"sha":"head"}});
            assert_eq!(
                hold_body(&plain, "next reason"),
                json!({"hold":"next reason"})
            );
            let mut poisoned = plain;
            poisoned["gate_submission"] = poison;
            assert_eq!(
                hold_body(&poisoned, "next reason"),
                json!({"hold":"next reason"}),
                "a raw hold was never lifted, so its head must remain"
            );
        }
    }

    /// The proof keys are written only when stated, so a car with no
    /// probe carries no `proof_probe: null` for a reader to trip on.
    #[test]
    fn proof_intent_writes_only_what_was_stated() {
        assert!(proof_intent(None, None, None).is_empty());
        assert!(proof_intent(Some("  "), None, None).is_empty());
        let p = proof_intent(Some("curl -s x | grep y"), Some("y"), None);
        assert_eq!(p[PROOF_PROBE], "curl -s x | grep y");
        assert_eq!(p[PROOF_EXPECT], "y");
        assert!(!p.contains_key(PROOF_EVENT));
        let e = proof_intent(None, None, Some("event-bound — the next yard cancel"));
        assert_eq!(e.len(), 1);
        assert_eq!(e[PROOF_EVENT], "event-bound — the next yard cancel");
    }

    /// A GATE-RUN'S PARK KEYS BECOME THE CAR'S PROOF KEYS, one reading
    /// for both doors that refresh a car (backlog 79a17c7a): the
    /// auto-park handler and `boss rerail --finish`. A bare re-gate
    /// states none, and so writes none — the car's own probe stands.
    #[test]
    fn a_gate_runs_park_keys_are_read_as_the_cars_proof_intent() {
        let md = json!({
            PARK_PROBE: "git show HEAD:x | grep -c 'y'",
            PARK_EXPECT: "1",
            PARK_SUMMARY: "not proof",
        });
        let p = proof_intent_of_park(md.as_object().unwrap());
        assert_eq!(p.len(), 2, "{p:?}");
        assert_eq!(p[PROOF_PROBE], "git show HEAD:x | grep -c 'y'");
        assert_eq!(p[PROOF_EXPECT], "1");
        let obj = |v: Value| v.as_object().cloned().unwrap();
        let e = proof_intent_of_park(&obj(json!({ PARK_PROOF_EVENT: "event-bound" })));
        assert_eq!(e[PROOF_EVENT], "event-bound");
        assert!(proof_intent_of_park(&obj(json!({ "branch": "fix/x" }))).is_empty());
    }

    /// A PARK THAT LEAVES THE CAR UNPROVABLE IS REFUSED (backlog
    /// 031a3ca9: 65 of 81 landed cars owed a proof carried neither a
    /// probe nor an event, because no door refused them). The car it
    /// lands on counts: a bare re-gate of a probed car is a refresh, and
    /// of an unprobed one it is the same gap again.
    #[test]
    fn a_park_that_leaves_the_car_with_no_proof_is_refused() {
        let obj = |v: Value| v.as_object().cloned().unwrap();
        let none = serde_json::Map::new();
        let why = unproven_park(None, &none).expect("a bare park files an unprovable car");
        for named in [
            "--park-probe",
            "--park-expect",
            "--park-proof-event",
            "`probe`",
            "`expect`",
            "`proof_event`",
            "031a3ca9",
        ] {
            assert!(why.contains(named), "the refusal names {named}: {why}");
        }
        let probed = proof_intent(Some("git show HEAD:x | grep -c 'y'"), Some("1"), None);
        assert_eq!(unproven_park(None, &probed), None);
        let event = proof_intent(None, None, Some("event-bound — the next red train"));
        assert_eq!(unproven_park(None, &event), None);
        let expect_only = obj(json!({ PROOF_EXPECT: "1" }));
        assert!(
            unproven_park(None, &expect_only).is_some(),
            "a string with no command is not a proof"
        );

        let car_probed = json!({"metadata": { PROOF_PROBE: "echo ok", PROOF_EXPECT: "ok" }});
        let car_event = json!({"metadata": { PROOF_EVENT: "a stalled train" }});
        let car_bare = json!({"metadata": { "branch": "fix/x" }});
        let car_blank = json!({"metadata": { PROOF_PROBE: "  " }});
        assert_eq!(unproven_park(Some(&car_probed), &none), None);
        assert_eq!(unproven_park(Some(&car_event), &none), None);
        assert!(unproven_park(Some(&car_bare), &none).is_some());
        assert!(
            unproven_park(Some(&car_blank), &none).is_some(),
            "a blank probe is no probe"
        );
        assert_eq!(unproven_park(Some(&car_bare), &probed), None);
    }

    /// THE REFUSAL NAMES THE VERB ITS FLAGS BELONG TO (backlog 92a51856).
    /// One text serves four doors, and only `boss gate` takes a probe:
    /// printed by `boss park`, a bare `--park-probe` read as if that verb
    /// had the flag. And it says a car already carrying a probe keeps it,
    /// because the documented repair is a park-flagged re-gate.
    #[test]
    fn the_unproven_refusal_names_the_verb_its_flags_belong_to() {
        assert!(
            UNPROVEN_PARK.contains("boss gate <branch> --park-probe"),
            "{UNPROVEN_PARK}"
        );
        assert!(
            UNPROVEN_PARK.contains("boss park and boss rerail take none"),
            "{UNPROVEN_PARK}"
        );
        assert!(
            UNPROVEN_PARK.contains("keeps it"),
            "a probed car needs no restatement: {UNPROVEN_PARK}"
        );
    }

    /// THE CAR A PARK LANDS ON (backlog 92a51856): the one the auto-park
    /// handler refreshes (parked) or adopts (opened at build start), and
    /// none for a boarded car, another branch's, or no car at all.
    #[test]
    fn a_park_lands_on_the_parked_or_the_building_car() {
        let car = |id: &str, branch: &str, gate: &str, review: &str, train: Value| {
            json!({
                "id": id, "status": "open",
                "metadata": { "branch": branch, "train": train },
                "steps": [
                    {"spec_slug": GATE_SLUG, "title": GATE, "status": gate},
                    {"spec_slug": REVIEW_SLUG, "title": REVIEW, "status": review},
                ]
            })
        };
        let parked = car("parked", "fix/x", "completed", "ready", Value::Null);
        let building = car("building", "fix/x", "active", "pending", Value::Null);
        let aboard = car("aboard", "fix/x", "completed", "ready", json!("train-1"));
        let other = car("other", "fix/y", "completed", "ready", Value::Null);
        let id = |c: Option<&Value>| c.and_then(|c| c["id"].as_str()).map(str::to_string);

        let docked = [other.clone(), parked.clone()];
        assert_eq!(
            id(car_a_park_lands_on(&docked, "fix/x")),
            Some("parked".into())
        );
        let opened = [building.clone()];
        assert_eq!(
            id(car_a_park_lands_on(&opened, "fix/x")),
            Some("building".into())
        );
        assert_eq!(car_a_park_lands_on(&[aboard], "fix/x"), None);
        assert_eq!(car_a_park_lands_on(&[other], "fix/x"), None);
        assert_eq!(car_a_park_lands_on(&[], "fix/x"), None);
    }

    /// THE TIER STAMPS ARE COPIED, NOT REBUILT (ba429e7f): the set rides
    /// as the gate-run wrote it, an empty set included (a root-only
    /// change touched no tier, and that is a reading); a headline that
    /// is blank or a set that is not an array is left out, never nulled.
    #[test]
    fn tier_stamps_copy_the_set_and_headline_verbatim_and_omit_what_is_malformed() {
        let md = json!({
            "software_tiers": ["core", "frontend"],
            "software_tier": "core",
            "delivery_channel": "software",
        });
        let t = tier_stamps(md.as_object().unwrap());
        assert_eq!(t.len(), 2);
        assert_eq!(t[SOFTWARE_TIERS], json!(["core", "frontend"]));
        assert_eq!(t[SOFTWARE_TIER], "core");
        assert!(!t.contains_key("delivery_channel"));

        let empty = json!({ "software_tiers": [] });
        let t = tier_stamps(empty.as_object().unwrap());
        assert_eq!(t[SOFTWARE_TIERS], json!([]));
        assert!(!t.contains_key(SOFTWARE_TIER));

        let bad = json!({ "software_tiers": "core", "software_tier": " " });
        assert!(tier_stamps(bad.as_object().unwrap()).is_empty());
        assert!(tier_stamps(&serde_json::Map::new()).is_empty());
    }

    /// PROVENANCE WITHOUT THE CLOSE. Same write-only-what-was-stated
    /// contract as the proof keys — and the key that matters here is the
    /// one NOT written: `backlog_item` is what the arrival rule follows,
    /// so neither provenance key may collide with it.
    #[test]
    fn item_provenance_writes_only_what_was_stated_and_never_the_closing_edge() {
        assert!(item_provenance(None, None).is_empty());
        assert!(item_provenance(Some("  "), Some("\t")).is_empty());
        let p = item_provenance(Some("cf0f5e2d"), None);
        assert_eq!(p[PARTIAL_ITEM], "cf0f5e2d");
        assert!(!p.contains_key(NO_ITEM_REASON));
        assert!(
            !p.contains_key(BACKLOG_ITEM),
            "a partial edge must never write the key the arrival rule follows"
        );
        let n = item_provenance(None, Some("David asked for this in conversation"));
        assert_eq!(n.len(), 1);
        assert_eq!(n[NO_ITEM_REASON], "David asked for this in conversation");
        assert_ne!(PARTIAL_ITEM, BACKLOG_ITEM);
        assert_ne!(NO_ITEM_REASON, BACKLOG_ITEM);
    }

    /// EVERY OTHER ITEM, AS STAMPED (a994f533, the writer half). The
    /// gate stamps a list; the car gets the declared `also_answers` edge
    /// with blanks dropped and repeats folded, and nothing at all when
    /// the gate stated none — absent, never an empty list, so a re-gate
    /// that names none leaves a recorded list alone.
    #[test]
    fn also_answers_carries_every_stated_id_once_and_nothing_else() {
        assert!(also_answers(None).is_empty());
        assert!(also_answers(Some(&json!([]))).is_empty());
        assert!(also_answers(Some(&json!(["  ", ""]))).is_empty());
        assert!(
            also_answers(Some(&json!("5994de6d"))).is_empty(),
            "a bare string is not the list the gate stamps"
        );
        let a = also_answers(Some(&json!(["5994de6d", " ", "cab50f4c", "5994de6d"])));
        assert_eq!(a.len(), 1);
        assert_eq!(a[ALSO_ANSWERS], json!(["5994de6d", "cab50f4c"]));
        assert_ne!(PARK_ALSO_ANSWERS, ALSO_ANSWERS);
        assert_ne!(ALSO_ANSWERS, BACKLOG_ITEM);
    }

    #[test]
    fn the_car_body_carries_the_fields_the_api_demands() {
        let b = car_body(
            "feat/x",
            "A thing does the thing. And more.",
            None,
            None,
            "emp-owner",
        );
        assert!(
            b["metadata"].get("delivery_channel").is_none(),
            "no delivery_channel when None"
        );
        let d = car_body("feat/x", "A thing.", None, Some("data"), "emp-owner");
        assert_eq!(
            d["metadata"]["delivery_channel"], "data",
            "the car carries the delivery channel the gate stamped"
        );
        for f in [
            "kind", "subject", "title", "owner_id", "status", "priority", "metadata", "tags",
        ] {
            assert!(b.get(f).is_some(), "car body is missing `{f}`");
        }
        assert_eq!(b["title"], "A thing does the thing");
        assert_eq!(b["subject"]["id"], "feat/x");
        assert_eq!(b["owner_id"], "emp-owner", "the owner is the one handed in");
        assert!(b["metadata"].get(BACKLOG_ITEM).is_none());
    }

    /// THE THIRD KEY IS A CONSTANT TOO (backlog 973d353f). Three keys
    /// answer one question — which item this car answered — and
    /// `require_item_answer` guarantees exactly one of them is present.
    /// [`PARTIAL_ITEM`] and [`NO_ITEM_REASON`] were `pub const` from the
    /// day they were written; the third, and the commonest (187 of 200
    /// recent cars carried it, against 6 and 7), was a bare string in
    /// fourteen files. It had not drifted, which is why it was cheap to
    /// collapse — but the asymmetry was itself the hazard: a reader who
    /// finds two of three as constants concludes the pattern is
    /// constants, and will not grep for a literal spelling of the third.
    #[test]
    fn the_three_item_answer_keys_are_each_one_definition() {
        assert_eq!(BACKLOG_ITEM, "backlog_item", "the wire spelling is fixed");
        assert_ne!(BACKLOG_ITEM, PARTIAL_ITEM);
        assert_ne!(BACKLOG_ITEM, NO_ITEM_REASON);
        // The closing edge is the one the arrival rule follows, so the
        // provenance-only builder must still never write it.
        assert!(!item_provenance(Some("cf0f5e2d"), None).contains_key(BACKLOG_ITEM));
        let b = car_body("feat/x", "Summary", Some("de6f0c06"), None, "emp-owner");
        assert_eq!(b["metadata"][BACKLOG_ITEM], "de6f0c06");
    }

    #[test]
    fn a_backlog_edge_is_carried_when_given() {
        let b = car_body(
            "feat/x",
            "Summary",
            Some("de6f0c06-a341-4445-9f47-399dc27a60fb"),
            None,
            "emp-owner",
        );
        assert_eq!(
            b["metadata"][BACKLOG_ITEM],
            "de6f0c06-a341-4445-9f47-399dc27a60fb"
        );
    }

    #[test]
    fn every_step_park_fills_says_when_it_was_filled() {
        let receipt = Receipt {
            raw: GREEN.to_string(),
            head: HEAD.to_string(),
            mode: "full".to_string(),
        };
        let fields = step_fields("s", "e", "t", "v", &receipt, at("2026-08-29T04:15:09Z"));

        assert_eq!(fields.len(), 3);
        for (slug, title, md) in &fields {
            assert_eq!(
                md.get("completed_at").and_then(Value::as_str),
                Some("2026-08-29T04:15:09Z"),
                "{title} was filled without saying when"
            );
            assert!(
                !slug.is_empty(),
                "{title} carries the registry slug `find_step` looks it up by"
            );
        }
    }

    #[test]
    fn the_stamp_matches_the_one_the_conductor_writes() {
        // Dock queue time is `review.completed_at` (the conductor's)
        // minus `gate.completed_at` (this), so a format that differs by
        // writer is wrong only in the subtraction.
        let receipt = Receipt {
            raw: GREEN.to_string(),
            head: HEAD.to_string(),
            mode: "full".to_string(),
        };
        let now = at("2026-08-29T04:15:09.847213Z");
        let fields = step_fields("s", "e", "t", "v", &receipt, now);
        let parked = fields[2].2["completed_at"].as_str().unwrap().to_string();

        assert_eq!(parked, stamp(now));
        assert!(
            !parked.contains('.') && parked.ends_with('Z'),
            "sub-second precision and offsets both break string comparison \
             against the conductor's stamps: {parked}"
        );
    }

    #[test]
    fn parking_does_not_disturb_the_evidence_it_already_carried() {
        let receipt = Receipt {
            raw: GREEN.to_string(),
            head: HEAD.to_string(),
            mode: String::new(),
        };
        let f = step_fields(
            "sum",
            "exc",
            "tst",
            "ver",
            &receipt,
            at("2026-08-29T04:15:09Z"),
        );
        assert_eq!(f[0].2["summary"], json!("sum"));
        assert_eq!(f[0].2["excludes"], json!("exc"));
        assert_eq!(f[1].2["test"], json!("tst"));
        assert_eq!(f[2].2["verified"], json!("ver"));
        // An empty mode still reads as a full gate.
        assert_eq!(f[2].2["gates"], json!("full"));
        assert_eq!(f[2].2["receipt"], json!(GREEN));
    }
}

#[cfg(test)]
mod regate_tests {
    use super::*;

    /// THE ONE PREDICATE every reader that turns a green into a car, or
    /// refreshes one, asks first (backlog 3e63662c; the reviews of
    /// c9439bd1 and cc7273c0). A receipt carrying `estate` was written
    /// by a run that declared no estate, so its live comparisons were
    /// never made: refused, naming the declaration. A receipt that does
    /// not parse as a JSON object is refused too — whether it declared
    /// cannot be read, and an unread declaration is not an absent one.
    /// A plain cluster receipt passes.
    #[test]
    fn a_receipt_that_declares_no_estate_or_cannot_be_read_is_refused() {
        let declared = r#"{"verdict":"green","head":"abc","estate":{"declared":"none","live_not_run":["the-live-rules-are-the-authored-rules"]}}"#;
        let why = declared_no_estate(declared).expect("a declaring receipt is refused");
        assert!(
            why.contains("none") && why.contains("the-live-rules-are-the-authored-rules"),
            "the refusal names the declaration and the checks it skipped: {why}"
        );
        for garbage in ["not json at all", "[1,2]", "\"green\"", ""] {
            assert!(
                declared_no_estate(garbage).is_some(),
                "an unreadable receipt fails CLOSED: {garbage:?}"
            );
        }
        assert_eq!(
            declared_no_estate(r#"{"verdict":"green","head":"abc","checks":[]}"#),
            None,
            "a cluster receipt carries no `estate` and parks as before"
        );
    }

    /// THE SECOND DECLARATION (backlog 6665aa97; review 30397e74 of car
    /// 4f1a96ab). `BOSS_TRUNK=none` is the public mirror's too, behind
    /// the same runtime guard (infra/lint/lib/public-mirror.sh), and
    /// gate.sh writes `trunk` on its receipt. Every mirror receipt
    /// carries both today, so a receipt carrying `trunk` alone is a
    /// leak past that guard — refused exactly as an `estate` one is,
    /// naming the declaration and the lints that judged no car.
    #[test]
    fn a_receipt_that_declares_no_trunk_is_refused_as_one_that_declares_no_estate_is() {
        let trunk_only = r#"{"verdict":"green","head":"abc","trunk":{"declared":"none","not_judged":["factory-vocabulary"]}}"#;
        let why = declared_no_estate(trunk_only).expect("a trunk-declaring receipt is refused");
        assert!(
            why.contains("no per-car trunk")
                && why.contains("trunk-ref.sh")
                && why.contains("factory-vocabulary"),
            "the refusal names the declaration and the lints that judged no car: {why}"
        );
        let both = r#"{"verdict":"green","head":"abc","estate":{"declared":"none","live_not_run":[]},"trunk":{"declared":"none","not_judged":[]}}"#;
        assert!(
            declared_no_estate(both).is_some(),
            "a mirror receipt, carrying both, is refused"
        );
    }

    const GREEN: &str = r#"{"verdict": "green", "head": "0123456789abcdef0123456789abcdef01234567", "mode": "full", "fails": []}"#;

    fn receipt() -> Receipt {
        Receipt {
            raw: GREEN.to_string(),
            head: "0123456789abcdef0123456789abcdef01234567".to_string(),
            mode: "full".to_string(),
        }
    }

    /// A car as the jobs API lists it: open, branch in metadata, review
    /// step waiting — the shape both parkers and the conductor read.
    fn car(id: &str, branch: &str, review_status: &str, train: Value) -> Value {
        json!({
            "id": id,
            "kind": "ship-a-change",
            "status": "open",
            "metadata": { "branch": branch, "train": train },
            "steps": [
                {"spec_slug": "gate", "title": GATE, "status": "completed"},
                {"spec_slug": "review", "title": REVIEW, "status": review_status},
            ]
        })
    }

    #[test]
    fn a_car_waiting_at_review_with_no_train_is_parked() {
        assert!(is_parked(&car("c1", "fix/x", "ready", Value::Null)));
        assert!(is_parked(&car("c1", "fix/x", "active", Value::Null)));
    }

    #[test]
    fn a_boarded_car_is_not_parked() {
        // The conductor stamps `metadata.train` when a car boards and
        // clears it (Null) when a cancelled train releases the car.
        assert!(!is_parked(&car("c1", "fix/x", "ready", json!("train-1"))));
        assert!(is_parked(&car("c1", "fix/x", "ready", json!(""))));
    }

    #[test]
    fn a_car_past_review_is_not_parked() {
        assert!(!is_parked(&car("c1", "fix/x", "completed", Value::Null)));
        assert!(!is_parked(&car("c1", "fix/x", "skipped", Value::Null)));
        assert!(!is_parked(&car("c1", "fix/x", "pending", Value::Null)));
    }

    #[test]
    fn a_car_naming_no_branch_is_not_parked() {
        assert!(!is_parked(&car("c1", "", "ready", Value::Null)));
    }

    #[test]
    fn the_parked_car_for_a_branch_is_the_one_still_at_the_dock() {
        let cars = vec![
            car("boarded", "fix/x", "ready", json!("train-9")),
            car("other", "fix/y", "ready", Value::Null),
            car("parked", "fix/x", "ready", Value::Null),
        ];
        let got = parked_car_for(&cars, "fix/x").expect("one car is parked");
        assert_eq!(got["id"], "parked");
        assert!(parked_car_for(&cars, "fix/z").is_none());
    }

    #[test]
    fn the_regate_patch_copies_the_receipt_verbatim_and_clears_the_skip() {
        let p = regate_patch(&receipt(), "why", None);
        // VERBATIM: the receipt string, not a rebuilt object.
        assert_eq!(p["regate_receipt"], json!(GREEN));
        // Present-and-null: the metadata door DELETES a null key, which
        // is how the conductor's "left behind" reason goes away.
        assert!(p.get("skip_reason").is_some_and(Value::is_null));
        assert_eq!(p["regate_note"], json!("why"));
        // And the dock's left-behind streak goes with it (backlog
        // 2fccbfd6; the re-review of car 5eb1967e, C): a re-parked car
        // has a fresh receipt, so the streak it was held through is over.
        assert!(p.get(LEFT_BEHIND_TRAINS).is_some_and(Value::is_null), "{p}");
        // ...and its alarm stamp with it (round-2 re-review, N2): a stamp
        // that outlived its streak would make the next streak never alarm.
        assert!(p.get(LEFT_BEHIND_ALARM).is_some_and(Value::is_null), "{p}");
    }

    /// A re-parked car's count of consecutive assembly refusals ends with
    /// the refusal's reason (backlog 7e941603). Measured on car d89bafeb,
    /// 2026-09-28: `boss rerail --finish` landed a green `regate_receipt`
    /// and nulled `skip_reason`, but `skips` stayed 13 through four more
    /// conductor writes, so orient drew it SKIPPED REPEATEDLY with "no
    /// reason recorded" until it boarded. Present-and-NULL, not zero —
    /// boarding clears it the same way, and `next_skip_count` reads an
    /// absent key as none, so the next refusal counts one again.
    #[test]
    fn the_regate_patch_clears_the_skip_count_with_the_skip_reason() {
        let p = regate_patch(&receipt(), "why", None);
        assert!(p.get("skips").is_some_and(Value::is_null), "{p}");
    }

    /// The re-gate's prose rides the job under `regate_*`, trimmed,
    /// and an empty field is absent rather than null — a null key is
    /// deleted by the metadata door.
    #[test]
    fn the_regate_prose_rides_the_job_and_omits_what_was_not_said() {
        let m = regate_prose(" rebuilt: now does X ", "not Y", "", "   ");
        assert_eq!(m["regate_summary"], "rebuilt: now does X");
        assert_eq!(m["regate_excludes"], "not Y");
        assert!(!m.contains_key("regate_test"), "{m:?}");
        assert!(!m.contains_key("regate_verified"), "{m:?}");
        assert!(regate_prose("", "", "", "").is_empty());
    }

    #[test]
    fn the_regate_patch_carries_the_delivery_channel_when_known() {
        // A re-gate re-classifies and stamps the channel, so a rebased
        // or rerailed car is counted in the yard's mixes like a fresh one.
        let p = regate_patch(&receipt(), "why", Some("config"));
        assert_eq!(p["delivery_channel"], "config");
        // Unknown diff: OMITTED, never nulled — a null key is deleted by
        // the metadata door, which would strip a channel already on the car.
        let q = regate_patch(&receipt(), "why", None);
        assert!(
            q.get("delivery_channel").is_none(),
            "no delivery_channel key when the diff could not be classified"
        );
    }
}

#[cfg(test)]
mod landed_tests {
    use super::*;

    const BRANCH: &str = "fix/boot-never-refuses-over-an-unviable-workflow";

    /// The car that carried the branch on train #259, as the SoR holds it.
    fn landed() -> Value {
        json!({
            "id": "670087f4-0000-0000-0000-000000000000",
            "kind": "ship-a-change",
            "status": "closed",
            "metadata": {
                "branch": BRANCH, "merged": "true", "merge_ref": "b641f3adcf47",
                "outcome": "merged", "train": "3a476b50-7a3a-409f-a9bc-59266e44f331",
                "boarded_head": "a56b4a9"
            }
        })
    }

    /// The twin auto-park filed for the same branch, abandoned by hand.
    fn abandoned_twin() -> Value {
        json!({
            "id": "dfb98d07-0000-0000-0000-000000000000",
            "kind": "ship-a-change",
            "status": "closed",
            "metadata": { "branch": BRANCH, "outcome": "abandoned", "abandoned": "true" }
        })
    }

    #[test]
    fn a_closed_merged_car_is_the_landing() {
        let cars = vec![abandoned_twin(), landed()];
        let got = landed_car_for(&cars, BRANCH).expect("the merged car answers");
        assert_eq!(got["metadata"]["merge_ref"], "b641f3adcf47");
    }

    #[test]
    fn an_abandoned_car_is_spent_not_landed() {
        assert!(landed_car_for(&[abandoned_twin()], BRANCH).is_none());
    }

    /// The conductor stamps `merged` first and the dispatcher closes
    /// the Job after — the marker alone is a landing.
    #[test]
    fn the_merged_marker_counts_before_the_close() {
        let mut marked = landed();
        marked["status"] = json!("open");
        marked["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("outcome");
        assert!(is_landed(&marked));
        marked["metadata"]["merged"] = json!(true);
        assert!(is_landed(&marked));
        marked["metadata"]["merged"] = json!("false");
        assert!(!is_landed(&marked), "an explicit false is not a landing");
    }

    /// A LANDING MAIN LOST IS NOT A LANDING (backlog f9256445). Car
    /// dec3136a kept `merged: "true"` — the forge did merge it — and
    /// closed through `unlanded` when main moved back off the merge. Read
    /// as landed, its successor's re-gate would be skipped as "landed
    /// content" by the auto-park handler and refused by `boss gate`, and
    /// the work could never ride again.
    #[test]
    fn a_car_whose_landing_main_lost_is_not_the_landing() {
        let mut unlanded = landed();
        unlanded["metadata"]["outcome"] = json!("unlanded");
        unlanded["metadata"]["merged"] = json!("true");
        unlanded["metadata"]["unlanded"] = json!("true");
        assert!(!is_landed(&unlanded));
        assert!(landed_car_for(&[unlanded.clone()], BRANCH).is_none());
        // Open, marked, and unlanded before the dispatcher closed it.
        unlanded["status"] = json!("open");
        unlanded["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("outcome");
        assert!(!is_landed(&unlanded));
    }

    #[test]
    fn a_parked_car_is_not_a_landing_and_another_branch_does_not_answer() {
        let parked = json!({
            "id": "p", "status": "open",
            "metadata": { "branch": BRANCH },
            "steps": [{"spec_slug": "review", "title": REVIEW, "status": "ready"}]
        });
        assert!(landed_car_for(&[parked], BRANCH).is_none());
        assert!(landed_car_for(&[landed()], "feat/other").is_none());
    }
}

#[cfg(test)]
mod open_car_tests {
    use super::*;

    const BRANCH: &str = "feat/a-human-only-step-refuses-an-agent";

    /// Car d08a6418 as it stood at 20:31:20 UTC on 2026-09-08: gated
    /// green, filed, and BOARDED train #274 twenty-six seconds earlier —
    /// `metadata.train` stamped, its review step still waiting.
    fn boarded() -> Value {
        json!({
            "id": "d08a6418-e7af-484b-82bf-ab043229bfd6",
            "kind": "ship-a-change",
            "status": "open",
            "metadata": {
                "branch": BRANCH,
                "boarded_head": "cd0c4f7bdadf2306a589c922f240a7ff963f70a7",
                "train": "d72ecdb9-c5a1-4d16-8a00-d934fd565002"
            },
            "steps": [
                {"spec_slug": "gate", "title": GATE, "status": "completed"},
                {"spec_slug": "review", "title": REVIEW, "status": "ready"},
            ]
        })
    }

    /// The twin the auto-park handler filed for the same branch 26
    /// seconds later — parked at the dock while the first rode the
    /// train, and abandoned by hand.
    fn twin() -> Value {
        json!({
            "id": "ad54e95c-b48a-4a4c-87f8-7aa8e4e19eff",
            "kind": "ship-a-change",
            "status": "open",
            "metadata": { "branch": BRANCH },
            "steps": [
                {"spec_slug": "gate", "title": GATE, "status": "completed"},
                {"spec_slug": "review", "title": REVIEW, "status": "ready"},
            ]
        })
    }

    #[test]
    fn a_train_stamp_is_what_makes_a_car_boarded() {
        assert!(is_boarded(&boarded()));
        assert!(!is_boarded(&twin()), "no train stamp is not boarded");
        // A released car (the conductor nulls the stamp when a train is
        // cancelled) is back at the dock, not aboard.
        let mut released = boarded();
        released["metadata"]["train"] = Value::Null;
        assert!(!is_boarded(&released));
        // A closed car is history, whatever it once carried.
        let mut closed = boarded();
        closed["status"] = json!("closed");
        assert!(!is_boarded(&closed));
    }

    /// THE MEASURED CASE (02165b1d). With a car aboard a train the
    /// branch already HAS its car: the boarded one answers, so nothing
    /// files a second.
    #[test]
    fn a_boarded_car_is_the_open_car_for_its_branch() {
        let aboard = [boarded()];
        let got = open_car_for(&aboard, BRANCH).expect("a boarded car is an open car");
        assert_eq!(got["id"], "d08a6418-e7af-484b-82bf-ab043229bfd6");
        // Both shapes present: the boarded one wins, so "is this branch
        // already in transit" answers the same whether or not a twin
        // was already filed.
        let both = [twin(), boarded()];
        let got = open_car_for(&both, BRANCH).expect("a car answers");
        assert_eq!(got["id"], "d08a6418-e7af-484b-82bf-ab043229bfd6");
        assert!(is_boarded(got));
    }

    #[test]
    fn a_parked_car_is_the_open_car_when_nothing_has_boarded() {
        let dock = [twin()];
        let got = open_car_for(&dock, BRANCH).expect("a parked car is an open car");
        assert_eq!(got["id"], "ad54e95c-b48a-4a4c-87f8-7aa8e4e19eff");
        assert!(is_parked(got));
    }

    #[test]
    fn a_branch_with_no_live_car_has_none() {
        assert!(open_car_for(&[], BRANCH).is_none());
        assert!(open_car_for(&[boarded()], "feat/other").is_none());
        // Past review with no train: spent history, not a live car.
        let mut done = twin();
        done["steps"][1]["status"] = json!("completed");
        assert!(open_car_for(&[done], BRANCH).is_none());
        // Closed, however it closed.
        let mut abandoned = twin();
        abandoned["status"] = json!("closed");
        abandoned["metadata"]["abandoned"] = json!("true");
        assert!(open_car_for(&[abandoned], BRANCH).is_none());
    }

    /// A CANCELLED CAR IS NOT LIVE EITHER. The jobs API has two terminal
    /// statuses and `is_open` read only one, so an abandoned twin a
    /// person cancelled — rather than closed — still answered "this
    /// branch has a live car", which is the 02165b1d defect with the
    /// other terminal word. Asserted on both shapes, because `is_open`
    /// gates the boarded lookup and the parked one separately.
    #[test]
    fn a_cancelled_car_is_not_a_live_car() {
        let mut cancelled = twin();
        cancelled["status"] = json!("cancelled");
        assert!(!is_open(&cancelled));
        assert!(open_car_for(&[cancelled.clone()], BRANCH).is_none());
        let mut cancelled_aboard = boarded();
        cancelled_aboard["status"] = json!("cancelled");
        assert!(!is_boarded(&cancelled_aboard));
        assert!(open_car_for(&[cancelled_aboard], BRANCH).is_none());
    }
}

/// A PARKED CAR STATES ITS ITEM'S ROUTE (backlog ca76d8f9).
#[cfg(test)]
mod park_triage_tests {
    use super::*;

    const BRANCH: &str = "fix/a-parked-car-triages-its-item";
    const CAR_ID: &str = "9442139b-1616-4b8d-8a7a-d1e34ff96486";

    /// A `backlog-item` as `--park-backlog-item` usually finds it:
    /// filed, un-triaged, every route still pending.
    fn untriaged_item() -> Value {
        json!({
            "id": "5942f205-0f0e-4a51-9a31-2f8f3b0b7a11",
            "kind": "backlog-item",
            "status": "open",
            "steps": [
                { "id": "s-filed", "spec_slug": "filed", "status": "completed", "metadata": {} },
                { "id": "s-triage", "spec_slug": "triage", "status": "ready",
                  "metadata": { "context_md": "filed by a builder" } },
                { "id": "s-build", "spec_slug": "build", "status": "pending", "metadata": {} },
            ],
        })
    }

    #[test]
    fn parking_a_car_against_an_untriaged_item_routes_it_to_build() {
        let w = triage_on_park(&untriaged_item(), CAR_ID, BRANCH)
            .expect("an un-triaged item gets the route its car states");
        assert_eq!(w.step_id, "s-triage");
        assert_eq!(w.status_body, json!({"status": "completed"}));
        assert_eq!(w.metadata["disposition"], DISPOSITION_BUILD);
        let evidence = w.metadata["evidence"].as_str().unwrap_or_default();
        assert!(
            evidence.contains("9442139b") && evidence.contains(BRANCH),
            "the evidence names the car and its branch: {evidence}"
        );
    }

    /// THE ROUTE RIDES THE MERGE DOOR (backlog e39a9d2a, Stage 1). This
    /// was one PUT of `{status, metadata}` whose metadata was the step's
    /// metadata AS THE PARK READ IT plus the two route keys. The step PUT
    /// replaces metadata wholesale, so any key written between that read
    /// and the PUT — a claim's lease, a person's note — was dropped by
    /// omission, silently. Now the two keys go through
    /// `PATCH …/steps/{id}/metadata`, one transaction against the row as
    /// it stands, and the PUT carries the status and nothing to drop. The
    /// step's own keys are therefore NOT in the body: the merge keeps
    /// them where they are, and a body that re-sent them would re-send a
    /// stale copy.
    #[test]
    fn the_park_route_merges_its_keys_and_puts_only_the_status() {
        for item in [untriaged_item(), untriaged_feedback()] {
            let w = triage_on_park(&item, CAR_ID, BRANCH).expect("an un-triaged packet routes");
            assert_eq!(
                w.status_body,
                json!({"status": "completed"}),
                "the PUT must carry the status alone"
            );
            let keys: Vec<&str> = w
                .metadata
                .as_object()
                .map(|m| m.keys().map(String::as_str).collect())
                .unwrap_or_default();
            assert_eq!(
                keys,
                ["disposition", "evidence"],
                "the merge body carries the route and nothing read off the step"
            );
            assert_eq!(
                w.merge_path("item-1"),
                format!("/api/jobs/item-1/steps/{}/metadata", w.step_id)
            );
            assert_eq!(
                w.status_path("item-1"),
                format!("/api/jobs/item-1/steps/{}", w.step_id)
            );
        }
    }

    /// The idempotence that makes this safe to run on every re-gate and
    /// every refresh: a triage a person already completed is a decision,
    /// and a park never overwrites it.
    #[test]
    fn an_already_triaged_item_is_left_exactly_alone() {
        let mut done = untriaged_item();
        done["steps"][1]["status"] = json!("completed");
        done["steps"][1]["metadata"] = json!({ "disposition": "verify", "evidence": "by hand" });
        assert!(triage_on_park(&done, CAR_ID, BRANCH).is_none());

        // A disposition written while the step is somehow still open is
        // a decision too.
        let mut decided = untriaged_item();
        decided["steps"][1]["metadata"] = json!({ "disposition": "decline" });
        assert!(triage_on_park(&decided, CAR_ID, BRANCH).is_none());

        // And a route nobody has opened is not one a park may complete.
        let mut pending = untriaged_item();
        pending["steps"][1]["status"] = json!("pending");
        assert!(triage_on_park(&pending, CAR_ID, BRANCH).is_none());
    }

    /// Claimed by a person is still open — the same "open" the arrival
    /// rule's route reads, one definition of it across both halves.
    #[test]
    fn an_active_routing_step_is_still_open() {
        let mut active = untriaged_item();
        active["steps"][1]["status"] = json!("active");
        assert!(triage_on_park(&active, CAR_ID, BRANCH).is_some());
    }

    /// A `user-feedback` packet as the chrome bar files it: submitted,
    /// un-triaged, every branch still pending. Its triage vocabulary is
    /// `reproduce|design|build|duplicate|needs-info|decline`
    /// (infra/platform/workflows/user-feedback.toml), so `build` is a
    /// route it admits.
    fn untriaged_feedback() -> Value {
        json!({
            "id": "9827c699-3e49-4494-a812-d3ab5fa4bd69",
            "kind": "user-feedback",
            "status": "open",
            "steps": [
                { "id": "s-submitted", "spec_slug": "submitted", "status": "completed", "metadata": {} },
                { "id": "s-triage", "spec_slug": "triage", "status": "ready",
                  "metadata": { "finding": "a page for the codebase stats" } },
                { "id": "s-build", "spec_slug": "build", "status": "pending", "metadata": {} },
            ],
        })
    }

    /// A car parked against a user-feedback packet states its route at
    /// PARK time, not at merge (backlog a29c3687). Until this test the
    /// park covered `backlog-item` only and the packet sat un-triaged
    /// until the v4 merge route closed it — David's 9827c699 read as
    /// nobody's decision for seventy minutes with its car on the dock.
    #[test]
    fn parking_a_car_against_untriaged_feedback_routes_it_to_build() {
        let w = triage_on_park(&untriaged_feedback(), CAR_ID, BRANCH)
            .expect("un-triaged feedback gets the route its car states");
        assert_eq!(w.step_id, "s-triage");
        assert_eq!(w.status_body, json!({"status": "completed"}));
        assert_eq!(w.metadata["disposition"], DISPOSITION_BUILD);
        let evidence = w.metadata["evidence"].as_str().unwrap_or_default();
        assert!(
            evidence.contains(CAR_ID) && evidence.contains(BRANCH),
            "the evidence names the car and its branch: {evidence}"
        );
    }

    /// Feedback a person already routed — to `reproduce`, `design`, or
    /// anything else — is a decision, and a park never overwrites it;
    /// nor does it touch a packet whose triage is done and whose open
    /// step is further along.
    #[test]
    fn feedback_already_past_triage_is_left_exactly_alone() {
        let mut investigating = untriaged_feedback();
        investigating["steps"][1]["status"] = json!("completed");
        investigating["steps"][1]["metadata"] = json!({ "disposition": "reproduce" });
        investigating["steps"][2] = json!({ "id": "s-investigate", "spec_slug": "investigate",
            "status": "ready", "metadata": {} });
        assert!(triage_on_park(&investigating, CAR_ID, BRANCH).is_none());

        let mut decided = untriaged_feedback();
        decided["steps"][1]["metadata"] = json!({ "disposition": "needs-info" });
        assert!(triage_on_park(&decided, CAR_ID, BRANCH).is_none());
    }

    /// The park routes the two kinds a car may be parked against and no
    /// other: a `design-doc`, an `ops-request`, a `ship-a-change` with a
    /// step that happens to be called `triage` is not a park's to decide.
    #[test]
    fn a_packet_of_any_other_kind_is_untouched() {
        for kind in ["design-doc", "ops-request", "ship-a-change", "gate-run"] {
            let mut other = untriaged_item();
            other["kind"] = json!(kind);
            assert!(triage_on_park(&other, CAR_ID, BRANCH).is_none(), "{kind}");
        }
        let mut kindless = untriaged_item();
        if let Some(m) = kindless.as_object_mut() {
            m.remove("kind");
        }
        assert!(triage_on_park(&kindless, CAR_ID, BRANCH).is_none());
    }

    #[test]
    fn a_terminal_or_stepless_item_is_untouched() {
        for terminal in ["closed", "cancelled"] {
            let mut gone = untriaged_item();
            gone["status"] = json!(terminal);
            assert!(
                triage_on_park(&gone, CAR_ID, BRANCH).is_none(),
                "{terminal}"
            );
        }
        let mut stepless = untriaged_item();
        stepless["steps"] = json!([]);
        assert!(triage_on_park(&stepless, CAR_ID, BRANCH).is_none());
        assert!(triage_on_park(&json!({}), CAR_ID, BRANCH).is_none());
    }
}

/// A CAR OPENS WHEN THE BUILD STARTS (backlog be025b44).
///
/// Until now a car's packet was filed by auto-park when the gate went
/// GREEN — the END of the build. Everything before that left no trace:
/// on 2026-09-08 three builder sessions died mid-flight and the only
/// symptom was a twin car appearing on the dock later, filed by a retry
/// loop that outlived its agent; on 2026-09-09 four builders ran for
/// 19–47 minutes each and the dock read empty throughout. The builder
/// now OPENS the car at build start, so a branch has exactly one packet
/// from its first minute and a green FINISHES that packet instead of
/// filing a second.
#[cfg(test)]
mod building_tests {
    use super::*;

    const BRANCH: &str = "feat/a-car-opens-when-the-build-starts";
    const HEAD: &str = "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8";
    const GREEN: &str = r#"{"verdict": "green", "head": "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8", "mode": "full", "fails": []}"#;

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().into()
    }

    fn receipt() -> Receipt {
        Receipt {
            raw: GREEN.to_string(),
            head: HEAD.to_string(),
            mode: "full".to_string(),
        }
    }

    /// A car as `boss car open` leaves it: filed, its trigger
    /// auto-completed by the POST, `scope` completed with the brief the
    /// builder was handed, `build` CLAIMED (active, assignee = the actor
    /// running the verb), gate and review still ahead of it.
    ///
    /// `subject_branch` is the branch it was FILED under — its Subject —
    /// and `branch` the one it CARRIES. `boss rerail` repoints the second
    /// and leaves the first, which is how three twins got filed on
    /// 2026-09-08; the shared predicates read `metadata.branch`.
    fn building(id: &str, subject_branch: &str, branch: &str) -> Value {
        json!({
            "id": id,
            "kind": "ship-a-change",
            "status": "open",
            "subject": {"subject_kind": "custom", "id": subject_branch},
            "metadata": {
                "branch": branch,
                "build_started_at": "2026-09-10T18:00:00Z",
                "built_by": "claude@algedonic.dev",
            },
            "steps": [
                {"id": "s-opened", "spec_slug": "opened", "title": OPENED,
                 "status": "completed"},
                {"id": "s-scope", "spec_slug": "scope", "title": SCOPE,
                 "status": "completed",
                 "metadata": {"summary": "s", "excludes": "e"}},
                {"id": "s-build", "spec_slug": "build", "title": BUILD,
                 "status": "active", "assignee_id": "claude@algedonic.dev"},
                {"id": "s-gate", "spec_slug": "gate", "title": GATE, "status": "pending"},
                {"id": "s-review", "spec_slug": "review", "title": REVIEW, "status": "pending"},
            ],
        })
    }

    /// A car as the POST alone leaves it — nothing completed but the
    /// trigger. The shape auto-park has always filed.
    fn fresh(id: &str, branch: &str) -> Value {
        json!({
            "id": id,
            "kind": "ship-a-change",
            "status": "open",
            "metadata": {"branch": branch},
            "steps": [
                {"id": "s-opened", "spec_slug": "opened", "title": OPENED,
                 "status": "completed"},
                {"id": "s-scope", "spec_slug": "scope", "title": SCOPE, "status": "ready"},
                {"id": "s-build", "spec_slug": "build", "title": BUILD, "status": "pending"},
                {"id": "s-gate", "spec_slug": "gate", "title": GATE, "status": "pending"},
                {"id": "s-review", "spec_slug": "review", "title": REVIEW, "status": "pending"},
            ],
        })
    }

    /// A GATE LAUNCH HANDS IN THE BUILD AND HOLDS THE GATE (backlog
    /// 68a8726e): build completed with the test, the gate step stamped
    /// with the gate-run BEFORE it is claimed — and a second launch on a
    /// car already in the gate only restamps it.
    #[test]
    fn a_gate_launch_completes_build_and_claims_the_gate_for_its_run() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-08T23:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let c = building("b1", BRANCH, BRANCH);
        let plan = gate_launch(&c, "gr-1", "abc123", "12 tests", now)
            .unwrap()
            .expect("a car at build moves");
        let build = plan.build.expect("build is handed in at launch");
        assert_eq!(build.step_id, "s-build");
        assert_eq!(build.metadata["test"], "12 tests");
        assert_eq!(build.status_body, json!({"status": "completed"}));
        assert_eq!(plan.gate_step, "s-gate");
        assert_eq!(plan.gate_merge[GATE_RUN], "gr-1");
        assert_eq!(plan.gate_merge["gated_head"], "abc123");
        assert!(plan.claim);

        let mut in_gate = c.clone();
        in_gate["steps"][2]["status"] = json!("completed");
        in_gate["steps"][3]["status"] = json!("active");
        let again = gate_launch(&in_gate, "gr-2", "def456", "t", now)
            .unwrap()
            .unwrap();
        assert!(again.build.is_none(), "a completed build is not resent");
        assert!(!again.claim, "an active gate is restamped, not reclaimed");
        assert_eq!(again.gate_merge[GATE_RUN], "gr-2");

        let mut past = in_gate;
        past["steps"][3]["status"] = json!("completed");
        assert!(gate_launch(&past, "gr-3", "x", "t", now).unwrap().is_none());
    }

    /// A NON-GREEN VERDICT HANDS THE GATE BACK, and only the gate-run
    /// that holds it can: a car another launch restamped is not moved.
    #[test]
    fn a_red_hands_back_only_the_gate_its_own_run_holds() {
        let mut c = building("b1", BRANCH, BRANCH);
        c["steps"][2]["status"] = json!("completed");
        c["steps"][3]["status"] = json!("active");
        c["steps"][3]["metadata"] = json!({GATE_RUN: "gr-1"});
        let cars = [c.clone()];
        assert_eq!(car_in_gate_run(&cars, "gr-1").unwrap()["id"], "b1");
        assert!(car_in_gate_run(&cars, "gr-other").is_none());
        let writes = gate_return(&c, "gr-1", "failed");
        assert_eq!(writes.len(), 2, "held by gr-1: {writes:#?}");
        let (merge, put) = (&writes[0], &writes[1]);
        assert_eq!(
            (merge.method, merge.path.as_str()),
            ("PATCH", "/api/jobs/b1/steps/s-gate/metadata")
        );
        assert_eq!(merge.body[LAST_VERDICT]["verdict"], "failed");
        assert_eq!(merge.body[LAST_VERDICT][GATE_RUN], "gr-1");
        assert_eq!(
            (put.method, put.path.as_str()),
            ("PUT", "/api/jobs/b1/steps/s-gate")
        );
        assert_eq!(put.body, json!({"status": "ready"}));
        // R3: BOTH writes are judged on the holder; the PUT also on the
        // verdict the merge wrote — the state the judged merge produced.
        assert_eq!(
            merge.expect,
            json!({GATE_RUN: "gr-1"}).as_object().cloned().unwrap()
        );
        assert_eq!(
            Value::Object(put.expect.clone()),
            json!({GATE_RUN: "gr-1", LAST_VERDICT: {GATE_RUN: "gr-1", "verdict": "failed"}})
        );
        assert!(gate_return(&c, "gr-other", "failed").is_empty());
        c["steps"][3]["status"] = json!("ready");
        assert!(
            gate_return(&c, "gr-1", "failed").is_empty(),
            "already handed back"
        );
    }

    /// A car a launch has moved into its gate, held by `gate_run` at HEAD.
    fn held(gate_run: &str) -> Value {
        let mut c = building("b1", BRANCH, BRANCH);
        c["steps"][2]["status"] = json!("completed");
        c["steps"][3]["status"] = json!("active");
        c["steps"][3]["metadata"] = json!({GATE_RUN: gate_run, "gated_head": HEAD});
        c
    }

    fn edge() -> crate::fulfils::Fulfils {
        crate::fulfils::Fulfils {
            job: "b1".into(),
            step: "s-gate".into(),
        }
    }

    /// THE EDGE IS READ BACK OFF THE STAMP (design 689d1f7f car 2,
    /// backlog a3a367e4): a launch writes it only for a car whose gate
    /// step names its gate-run, so a lost fence or a newer launch leaves
    /// the gate-run without one rather than pointing at a step it does
    /// not hold.
    #[test]
    fn a_launch_writes_the_edge_only_onto_the_gate_its_run_holds() {
        assert_eq!(gate_edge(&held("gr-1"), "gr-1"), Some(edge()));
        assert_eq!(gate_edge(&held("gr-2"), "gr-1"), None, "restamped");
        assert_eq!(gate_edge(&building("b1", BRANCH, BRANCH), "gr-1"), None);
        let mut closed = held("gr-1");
        closed["status"] = json!("closed");
        assert_eq!(gate_edge(&closed, "gr-1"), None);
    }

    /// THE CUTOVER, OLD DISPATCHER (design 689d1f7f car 2). The CLI and
    /// the dispatcher deploy apart, so a dispatcher that predates the
    /// edge will read an edged gate-run's verdict. The edge is ADDITIVE:
    /// the car it names is the one the scan already finds — by its branch
    /// on a green, by the stamp on a red — so that dispatcher still
    /// finishes it, or hands it back, as before.
    #[test]
    fn an_edged_launch_leaves_the_car_the_scan_still_finds() {
        let c = held("gr-1");
        let e = gate_edge(&c, "gr-1").expect("the launch writes its edge");
        let cars = [c.clone()];
        assert_eq!(building_car_for(&cars, BRANCH).unwrap()["id"], e.job);
        assert_eq!(car_in_gate_run(&cars, "gr-1").unwrap()["id"], e.job);
    }

    /// A GREEN BY EDGE finishes only a gate its run holds — never an
    /// unbound one, which the scan's binding accepts (the edge carries no
    /// branch to vouch for it) — and a finished gate is not finished
    /// twice.
    #[test]
    fn a_green_by_edge_finishes_only_a_gate_its_run_holds() {
        match edge_green(&held("gr-1"), &edge(), BRANCH, "gr-1", None, HEAD) {
            Ok(EdgeGreen::Finish(expect)) => {
                assert_eq!(expect[GATE_RUN], "gr-1");
                assert_eq!(expect["gated_head"], HEAD);
            }
            other => panic!("its own gate: {other:?}"),
        }
        let stale = edge_green(&held("gr-2"), &edge(), BRANCH, "gr-1", None, HEAD).unwrap_err();
        assert!(stale.contains("held by gate-run gr-2"), "{stale}");
        let mut unbound = building("b1", BRANCH, BRANCH);
        unbound["steps"][3]["status"] = json!("ready");
        let why = edge_green(&unbound, &edge(), BRANCH, "gr-1", None, HEAD).unwrap_err();
        assert!(why.contains("names no gate-run"), "{why}");
        let mut done = held("gr-1");
        done["steps"][3]["status"] = json!("completed");
        assert_eq!(
            edge_green(&done, &edge(), BRANCH, "gr-1", None, HEAD),
            Ok(EdgeGreen::Finished)
        );
    }

    /// AN EDGE THAT DOES NOT NAME AN OPEN CAR'S GATE is refused on both
    /// verdicts, naming why — never resolved some other way.
    #[test]
    fn an_edge_to_anything_but_an_open_cars_gate_is_refused() {
        let wrong_step = crate::fulfils::Fulfils {
            job: "b1".into(),
            step: "s-review".into(),
        };
        let other_car = crate::fulfils::Fulfils {
            job: "b2".into(),
            step: "s-gate".into(),
        };
        let mut closed = held("gr-1");
        closed["status"] = json!("closed");
        for (car, e, says) in [
            (held("gr-1"), &wrong_step, "is not car"),
            (held("gr-1"), &other_car, "is not that car"),
            (closed, &edge(), "no longer open"),
        ] {
            let green = edge_green(&car, e, BRANCH, "gr-1", None, HEAD).unwrap_err();
            assert!(green.contains(says), "{green}");
            let red = edge_return(&car, e, "gr-1", "failed").unwrap_err();
            assert!(red.contains(says), "{red}");
        }
    }

    /// A GREEN REPLAYED AFTER ITS CAR CLOSED reads Finished, not an
    /// ended-car refusal (review 01fe1c71 of car 3a7ab114, finding 3):
    /// the step is judged before the car's status, because a landed car's
    /// gate was completed by the very green being replayed. A car that
    /// ended with its gate still OPEN is still refused — no verdict
    /// finishes an ended car.
    #[test]
    fn a_green_replayed_after_its_car_closed_reads_finished() {
        let mut landed = held("gr-1");
        landed["steps"][3]["status"] = json!("completed");
        landed["status"] = json!("closed");
        assert_eq!(
            edge_green(&landed, &edge(), BRANCH, "gr-1", None, HEAD),
            Ok(EdgeGreen::Finished)
        );
        let mut abandoned = held("gr-1");
        abandoned["status"] = json!("cancelled");
        let why = edge_green(&abandoned, &edge(), BRANCH, "gr-1", None, HEAD).unwrap_err();
        assert!(why.contains("no longer open"), "{why}");
    }

    /// AN EDGE IS AS STRICT AS THE SCAN ABOUT THE BRANCH (review 01fe1c71
    /// of car 3a7ab114, finding 4): a green from a gate-run that gated
    /// another branch finishes nothing, even holding the step at the head
    /// — including through a relaunch naming this car's holder.
    #[test]
    fn a_green_by_edge_finishes_only_the_car_of_the_branch_it_gated() {
        let why = edge_green(&held("gr-1"), &edge(), "feat/other", "gr-1", None, HEAD).unwrap_err();
        assert!(why.contains("branch"), "{why}");
        assert!(why.contains("feat/other"), "{why}");
        let relaunch = edge_green(
            &held("gr-1"),
            &edge(),
            "feat/other",
            "gr-9",
            Some("gr-1"),
            HEAD,
        );
        assert!(relaunch.is_err(), "{relaunch:?}");
        assert!(matches!(
            edge_green(&held("gr-1"), &edge(), BRANCH, "gr-9", Some("gr-1"), HEAD),
            Ok(EdgeGreen::Finish(_))
        ));
    }

    /// A RED BY EDGE NEVER COMPLETES THE STEP (the trust boundary of
    /// backlog a3a367e4): it is the same judged hand-back to READY as the
    /// scan's, and nothing at all once another launch holds the step.
    #[test]
    fn a_red_by_edge_hands_back_and_never_completes() {
        let writes = edge_return(&held("gr-1"), &edge(), "gr-1", "failed").unwrap();
        assert_eq!(writes, gate_return(&held("gr-1"), "gr-1", "failed"));
        assert_eq!(writes.len(), 2);
        assert!(
            writes
                .iter()
                .all(|w| w.body.get("status") != Some(&json!("completed"))),
            "{writes:#?}"
        );
        assert!(
            edge_return(&held("gr-2"), &edge(), "gr-1", "failed")
                .unwrap()
                .is_empty()
        );
    }

    /// R2 (Codex review of car 0d04ee3e): a green finishes the gate step
    /// only for the gate-run that holds it, at the head it was launched
    /// at — and a car with no binding (filed before launches stamped one)
    /// is finished exactly as before.
    #[test]
    fn a_green_finishes_only_the_gate_its_run_holds_at_its_head() {
        let mut c = building("b1", BRANCH, BRANCH);
        assert_eq!(
            green_binding(&c, "gr-1", None, "abc1234"),
            Ok(unbound_gate()),
            "unbound: the write expects the binding still absent"
        );
        c["steps"][2]["status"] = json!("completed");
        c["steps"][3]["status"] = json!("active");
        c["steps"][3]["metadata"] = json!({GATE_RUN: "gr-2", "gated_head": "def5678aa"});
        let held = green_binding(&c, "gr-2", None, "def5678aa").expect("the holder at its head");
        assert_eq!(
            Value::Object(held),
            json!({GATE_RUN: "gr-2", "gated_head": "def5678aa"})
        );
        assert!(
            green_binding(&c, "gr-2", None, "def5678").is_ok(),
            "an abbreviation of the launched head is that head"
        );
        let stale = green_binding(&c, "gr-1", None, "abc1234").unwrap_err();
        assert!(stale.contains("held by gate-run gr-2"), "{stale}");
        let moved = green_binding(&c, "gr-2", None, "0000000").unwrap_err();
        assert!(moved.contains("another head"), "{moved}");
        // Handed back after a red, the step still names its last holder:
        // an older gate's late green is not that holder's verdict either.
        c["steps"][3]["status"] = json!("ready");
        assert!(green_binding(&c, "gr-1", None, "abc1234").is_err());
        c["steps"][3]["status"] = json!("completed");
        assert_eq!(
            green_binding(&c, "gr-1", None, "abc1234"),
            Ok(unbound_gate()),
            "past it"
        );
    }

    /// A LOST GATE'S RELAUNCH SUCCEEDS IT (backlog 64d417ca): the green
    /// of the run whose `relaunch_of` names the holder finishes the step
    /// that holder held, judged on the holder still holding it at its
    /// head — and nothing wider: a relaunch of another run, a relaunch
    /// vouching for another head, or a step a newer launch restamped
    /// after the loss is not the relaunch's to finish.
    #[test]
    fn a_lost_gates_relaunch_finishes_the_gate_its_lost_run_held() {
        let mut c = building("b1", BRANCH, BRANCH);
        c["steps"][2]["status"] = json!("completed");
        // The loss handed the step back READY, still naming the lost run
        // — car 88645b17's shape on 2026-11-10.
        c["steps"][3]["status"] = json!("ready");
        c["steps"][3]["metadata"] = json!({
            GATE_RUN: "gr-lost", "gated_head": "544327d0aa",
            LAST_VERDICT: {GATE_RUN: "gr-lost", "verdict": "lost"},
        });
        assert!(
            green_binding(&c, "gr-relaunch", None, "544327d0aa").is_err(),
            "a green that names no predecessor is still held elsewhere"
        );
        let took = green_binding(&c, "gr-relaunch", Some("gr-lost"), "544327d0aa")
            .expect("the relaunch takes over the lost run's binding");
        assert_eq!(
            Value::Object(took),
            json!({GATE_RUN: "gr-lost", "gated_head": "544327d0aa"}),
            "judged on the lost holder still holding the step at its head"
        );
        let stranger =
            green_binding(&c, "gr-relaunch", Some("gr-other"), "544327d0aa").unwrap_err();
        assert!(stranger.contains("held by gate-run gr-lost"), "{stranger}");
        let moved = green_binding(&c, "gr-relaunch", Some("gr-lost"), "0000000").unwrap_err();
        assert!(moved.contains("another head"), "{moved}");
        // A launch after the loss restamped the step: its holder is not
        // the lost run, so the relaunch's green finishes nothing.
        c["steps"][3]["status"] = json!("active");
        c["steps"][3]["metadata"] = json!({GATE_RUN: "gr-new", "gated_head": "1234567aa"});
        assert!(green_binding(&c, "gr-relaunch", Some("gr-lost"), "544327d0aa").is_err());
    }

    /// THE STEP NAMES THE RUN WHOSE RECEIPT IT CARRIES (backlog
    /// 84e46932): a relaunch's green renames the gate step's holder to
    /// itself in the evidence merge, keeping the lost run beside it, and
    /// the PUT is judged on that renamed state; the holder's own green
    /// writes exactly the evidence, judged as before.
    #[test]
    fn a_relaunchs_green_renames_the_gate_steps_holder_in_its_merge() {
        let gate = StepWrite::completing("s-gate", GATE, json!({"receipt": "r"}));
        let held = serde_json::Map::from_iter([
            (GATE_RUN.to_string(), json!("gr-lost")),
            ("gated_head".to_string(), json!("544327d0aa")),
        ]);
        let [merge, put] = green_gate_writes("car-1", &gate, &held, "gr-relaunch");
        assert_eq!(merge.path, "/api/jobs/car-1/steps/s-gate/metadata");
        assert_eq!(
            merge.body,
            json!({"receipt": "r", GATE_RUN: "gr-relaunch", LOST_GATE_RUN: "gr-lost"})
        );
        assert_eq!(
            merge.expect, held,
            "the merge lands only on the lost holder"
        );
        assert_eq!(put.path, "/api/jobs/car-1/steps/s-gate");
        assert_eq!(put.body, json!({"status": "completed"}));
        assert_eq!(
            Value::Object(put.expect),
            json!({GATE_RUN: "gr-relaunch", LOST_GATE_RUN: "gr-lost", "gated_head": "544327d0aa"}),
            "the PUT lands only on the state the merge made"
        );

        // The holder's own green, and an unbound car: no rename.
        for expect in [
            serde_json::Map::from_iter([(GATE_RUN.to_string(), json!("gr-relaunch"))]),
            unbound_gate(),
        ] {
            let [merge, put] = green_gate_writes("car-1", &gate, &expect, "gr-relaunch");
            assert_eq!(merge.body, json!({"receipt": "r"}));
            assert_eq!((merge.expect, put.expect), (expect.clone(), expect));
        }
    }

    /// R1 (Codex review of car 0d04ee3e): a run's car is found by the
    /// run FIRST, whatever branch it now carries; the branch only after.
    #[test]
    fn a_run_finds_its_own_car_before_the_branch_and_refuses_to_guess() {
        let mut c = building("b1", BRANCH, BRANCH);
        c["metadata"]["agent_run"] = json!("run-1");
        c["metadata"]["backlog_item"] = json!("item-1");
        let cars = [c.clone()];
        // Renamed: the run's car, to be re-pointed.
        assert!(matches!(
            run_car_for(&cars, Some("run-1"), Some("item-1"), "feat/renamed"),
            Ok(RunCar::Run(r)) if r["id"] == "b1"
        ));
        // Same branch: the branch's car, nothing to re-point.
        assert!(matches!(
            run_car_for(&cars, Some("run-1"), Some("item-1"), BRANCH),
            Ok(RunCar::Branch(b)) if b["id"] == "b1"
        ));
        // A repair — another run of the SAME change — takes the branch's car.
        assert!(matches!(
            run_car_for(&cars, Some("run-2"), Some("item-1"), BRANCH),
            Ok(RunCar::Branch(_))
        ));
        // Another run of ANOTHER change is refused, not stamped.
        let foreign = run_car_for(&cars, Some("run-9"), Some("item-9"), BRANCH).unwrap_err();
        assert!(foreign.contains("admitted by run run-1"), "{foreign}");
        assert!(run_car_for(&cars, None, None, BRANCH).is_err());
        assert_eq!(
            run_car_for(&cars, Some("run-9"), Some("item-9"), "feat/other"),
            Ok(RunCar::Neither)
        );
        // A hand-opened car names no run: the branch's car, as before.
        let hand = [building("h1", BRANCH, BRANCH)];
        assert!(matches!(
            run_car_for(&hand, Some("run-9"), None, BRANCH),
            Ok(RunCar::Branch(_))
        ));
        // Two building cars for one run: refused, naming both.
        let mut twin = c.clone();
        twin["id"] = json!("b2");
        twin["metadata"]["branch"] = json!("feat/renamed");
        let twins = [c.clone(), twin];
        let e = run_car_for(&twins, Some("run-1"), Some("item-1"), BRANCH).unwrap_err();
        assert!(
            e.contains("2 building cars") && e.contains("b1") && e.contains("b2"),
            "{e}"
        );
        // The run's car elsewhere while the asked branch has its own car.
        let mut other = building("o1", "feat/renamed", "feat/renamed");
        other["metadata"]["backlog_item"] = json!("item-1");
        let both = [c, other];
        assert!(run_car_for(&both, Some("run-1"), Some("item-1"), "feat/renamed").is_err());
    }

    /// R1, round 2 (Codex review of car 0d04ee3e): a branch carrying TWO
    /// building cars is ambiguous input, and the answer must not depend
    /// on list order. A repair run that owns neither — both admitted by
    /// earlier runs of the same change — is refused naming both, in
    /// either order; and so is the run that owns one of them, because
    /// "the branch's car" no longer names one car.
    #[test]
    fn a_branch_with_two_building_cars_is_refused_whichever_comes_first() {
        let car = |id: &str, run: &str| {
            let mut c = building(id, BRANCH, BRANCH);
            c["metadata"]["agent_run"] = json!(run);
            c["metadata"]["backlog_item"] = json!("item-1");
            c
        };
        let (a, b) = (car("a1", "run-1"), car("b2", "run-2"));
        for cars in [[a.clone(), b.clone()], [b.clone(), a.clone()]] {
            for run in ["run-3", "run-1", "run-2"] {
                let e = run_car_for(&cars, Some(run), Some("item-1"), BRANCH).unwrap_err();
                assert!(
                    e.contains("2 building cars") && e.contains("a1") && e.contains("b2"),
                    "{run}: {e}"
                );
            }
        }
        // A hand-opened twin beside an admitted car is the same ambiguity.
        let hand = building("h1", BRANCH, BRANCH);
        assert!(run_car_for(&[hand, a], None, None, BRANCH).is_err());
    }

    /// THE THIRD STATE A LIVE CAR CAN BE IN. `is_parked` and
    /// `is_boarded` covered every live car while cars were only ever
    /// filed at green; a car opened at build start is neither.
    #[test]
    fn a_car_opened_at_build_start_is_building_not_parked_and_not_boarded() {
        let c = building("b1", BRANCH, BRANCH);
        assert!(
            is_building(&c),
            "a car whose gate has not reported is building"
        );
        assert!(!is_parked(&c), "its review step is not open yet");
        assert!(!is_boarded(&c), "no train has stamped it");
        assert!(!is_landed(&c));
    }

    /// THE TWINNING FAILURE MODE, CLOSED. "No parked car" was read as
    /// "no car" three times (d052afad, 02165b1d, 6790175e) and each time
    /// a second car was filed. A building car must answer the same
    /// question, or opening cars early would invent a fourth twin.
    #[test]
    fn the_building_car_is_the_branchs_one_live_car() {
        let cars = [building("b1", BRANCH, BRANCH)];
        let got = open_car_for(&cars, BRANCH).expect("a building car is a live car");
        assert_eq!(got["id"], "b1");
        assert_eq!(
            building_car_for(&cars, BRANCH).and_then(|c| c["id"].as_str()),
            Some("b1")
        );
        assert!(building_car_for(&cars, "feat/other").is_none());
    }

    /// A car already at the dock or aboard a train still wins: those
    /// questions were measured first and their answers must not move.
    #[test]
    fn a_parked_or_boarded_car_still_answers_before_a_building_one() {
        let mut parked = building("p", BRANCH, BRANCH);
        parked["steps"][3]["status"] = json!("completed");
        parked["steps"][4]["status"] = json!("ready");
        let cars = [building("b1", BRANCH, BRANCH), parked];
        assert_eq!(open_car_for(&cars, BRANCH).unwrap()["id"], "p");

        let mut boarded = building("a", BRANCH, BRANCH);
        boarded["metadata"]["train"] = json!("train-9");
        let cars = [building("b1", BRANCH, BRANCH), boarded];
        assert_eq!(open_car_for(&cars, BRANCH).unwrap()["id"], "a");
        assert!(
            !is_building(&cars[1]),
            "a boarded car is aboard, whatever its gate step says"
        );
    }

    /// `boss rerail` repoints `metadata.branch` and leaves the Subject
    /// where the car was filed, so the predicate reads the branch the car
    /// CARRIES (backlog 6790175e).
    #[test]
    fn a_rerailed_building_car_answers_on_the_branch_it_carries() {
        let cars = [building("b1", BRANCH, "feat/rerailed")];
        assert_eq!(
            building_car_for(&cars, "feat/rerailed").and_then(|c| c["id"].as_str()),
            Some("b1")
        );
        assert!(
            building_car_for(&cars, BRANCH).is_none(),
            "the filing branch is history once a rerail repoints the car"
        );
    }

    #[test]
    fn a_gated_or_closed_or_unreadable_car_is_not_building() {
        let mut gated = building("b1", BRANCH, BRANCH);
        gated["steps"][3]["status"] = json!("completed");
        assert!(!is_building(&gated), "its gate has reported");

        let mut closed = building("b1", BRANCH, BRANCH);
        closed["status"] = json!("closed");
        assert!(!is_building(&closed));

        let mut nameless = building("b1", BRANCH, BRANCH);
        nameless["metadata"]["branch"] = json!("");
        assert!(!is_building(&nameless));

        // A packet whose gate step this version of boss cannot find is
        // not adopted: an unreadable shape must refuse, not guess.
        let mut stepless = building("b1", BRANCH, BRANCH);
        stepless["steps"] = json!([]);
        assert!(!is_building(&stepless));
    }

    /// WHAT AN OPEN WRITES: the brief, on `scope`, and nothing else. The
    /// build step is taken through the CLAIM door (Ready→Active as a
    /// compare-and-set, which records the claimant and refuses a second
    /// agent), not by a status PUT here.
    #[test]
    fn an_open_records_the_brief_on_scope_and_nothing_else() {
        let writes = open_writes(
            &fresh("f1", BRANCH),
            "does a thing",
            "not that",
            at("2026-09-10T18:00:00Z"),
        )
        .expect("a fresh car can be opened");
        assert_eq!(writes.len(), 1, "one write: scope");
        assert_eq!(writes[0].step_id, "s-scope");
        assert_eq!(writes[0].title, SCOPE);
        assert_eq!(writes[0].status_body, json!({"status": "completed"}));
        assert_eq!(writes[0].metadata["summary"], "does a thing");
        assert_eq!(writes[0].metadata["excludes"], "not that");
        assert_eq!(
            writes[0].metadata["completed_at"], "2026-09-10T18:00:00Z",
            "the same stamp format every other writer uses"
        );
    }

    /// IDEMPOTENT: re-running the open on a car whose scope is already
    /// declared writes nothing, so a builder re-invoking the verb does
    /// not hit the 409 a completed step returns for a metadata write.
    #[test]
    fn opening_a_car_whose_scope_is_already_declared_writes_nothing() {
        let writes = open_writes(
            &building("b1", BRANCH, BRANCH),
            "s",
            "e",
            at("2026-09-10T18:00:00Z"),
        )
        .expect("an already-opened car is readable");
        assert!(writes.is_empty(), "scope is already completed: {writes:?}");
    }

    /// WHAT A GREEN OWES AN OPENED CAR: the steps it has NOT already
    /// completed. The step API refuses a metadata write to a completed
    /// step, so a green that re-sent `scope` would 409 on every car a
    /// builder opened.
    #[test]
    fn finishing_an_opened_car_skips_the_scope_the_open_already_completed() {
        let writes = finish_writes(
            &building("b1", BRANCH, BRANCH),
            "s",
            "e",
            "ran the tests",
            "seen working",
            &receipt(),
            at("2026-09-10T18:30:00Z"),
        )
        .expect("an opened car can be finished");
        let titles: Vec<&str> = writes.iter().map(|w| w.title).collect();
        assert_eq!(titles, vec![BUILD, GATE], "scope is already declared");
        assert_eq!(writes[0].step_id, "s-build");
        assert_eq!(writes[0].metadata["test"], "ran the tests");
        assert_eq!(writes[1].step_id, "s-gate");
        assert_eq!(
            writes[1].metadata["receipt"], GREEN,
            "the receipt rides verbatim, as it does on a fresh car"
        );
        assert_eq!(writes[1].metadata["verified"], "seen working");
    }

    /// And the path auto-park has always taken is unchanged: a car it
    /// just POSTed has nothing completed, so all three steps are filled
    /// in the order the workflow runs them.
    #[test]
    fn finishing_a_fresh_car_writes_all_three_steps_in_order() {
        let writes = finish_writes(
            &fresh("f1", BRANCH),
            "s",
            "e",
            "t",
            "v",
            &receipt(),
            at("2026-09-10T18:30:00Z"),
        )
        .expect("a fresh car can be finished");
        let titles: Vec<&str> = writes.iter().map(|w| w.title).collect();
        assert_eq!(titles, vec![SCOPE, BUILD, GATE]);
        assert_eq!(writes[0].step_id, "s-scope");
        for w in &writes {
            assert_eq!(w.status_body, json!({"status": "completed"}));
            assert_eq!(
                w.metadata["completed_at"], "2026-09-10T18:30:00Z",
                "{} was filled without saying when",
                w.title
            );
        }
    }

    /// NO CAR WRITE PUTS METADATA (backlog e39a9d2a, car 2 of its plan).
    ///
    /// The step PUT REPLACES metadata wholesale, and the registry
    /// materializes keys onto every step at admission —
    /// `metadata_defaults`, `authority_role`, `station`, `audience`,
    /// `claimable` — so these writers, which build their bodies fresh with
    /// no read, shed every one of those keys on every `boss car open`,
    /// `boss park` and auto-park. The evidence rides the step MERGE door
    /// (`PATCH …/steps/{id}/metadata`, one transaction against the row as
    /// it stands, so it cannot race) and the completion is a PUT carrying
    /// the status and nothing else to drop.
    #[test]
    fn every_car_write_merges_its_evidence_and_puts_only_the_status() {
        let opened = open_writes(&fresh("f1", BRANCH), "s", "e", at("2026-09-10T18:00:00Z"))
            .expect("a fresh car can be opened");
        let finished = finish_writes(
            &fresh("f1", BRANCH),
            "s",
            "e",
            "t",
            "v",
            &receipt(),
            at("2026-09-10T18:30:00Z"),
        )
        .expect("a fresh car can be finished");
        assert!(!opened.is_empty() && finished.len() == 3);
        for w in opened.iter().chain(&finished) {
            assert_eq!(
                w.status_body,
                json!({"status": "completed"}),
                "{}: the PUT must carry the status alone — a metadata key in it \
                 replaces the step's stored keys wholesale",
                w.title
            );
            assert!(
                w.metadata.as_object().is_some_and(|m| !m.is_empty()),
                "{}: the evidence rides the merge body",
                w.title
            );
            assert!(
                w.metadata.get("status").is_none(),
                "{}: `status` on the merge door would be a metadata key named status",
                w.title
            );
            assert_eq!(
                w.merge_path("car-1"),
                format!("/api/jobs/car-1/steps/{}/metadata", w.step_id),
                "the merge door, addressed through the car the step is on"
            );
            assert_eq!(
                w.status_path("car-1"),
                format!("/api/jobs/car-1/steps/{}", w.step_id)
            );
        }
    }

    /// A SHAPE WE CANNOT READ REFUSES. An in-flight packet is pinned to
    /// the workflow version it was admitted under; if a car's steps do
    /// not answer to the slugs this code knows, filling it by guesswork
    /// would write evidence onto the wrong step.
    #[test]
    fn finishing_refuses_a_car_whose_steps_it_cannot_find() {
        let mut odd = fresh("f1", BRANCH);
        odd["steps"] = json!([{"id": "s-opened", "spec_slug": "opened", "title": OPENED,
                               "status": "completed"}]);
        let e = finish_writes(
            &odd,
            "s",
            "e",
            "t",
            "v",
            &receipt(),
            at("2026-09-10T18:30:00Z"),
        )
        .expect_err("a car with no scope step cannot be finished");
        assert!(
            e.contains("scope"),
            "the refusal names the missing step: {e}"
        );
        let e = open_writes(&odd, "s", "e", at("2026-09-10T18:00:00Z"))
            .expect_err("nor can it be opened");
        assert!(e.contains("scope"), "{e}");
    }

    /// THE MIDDLE THIRD NEEDS TO KNOW WHO AND WHERE. An agent working
    /// for forty minutes left no trace at all; the fact a board renders
    /// is the actor, the host and the worktree, stamped when the build
    /// started.
    #[test]
    fn the_build_start_stamp_names_the_actor_the_host_and_the_worktree() {
        let md = build_start(
            "claude@algedonic.dev",
            "boss-dev-0",
            "/work/boss/.claude/worktrees/agent-a23",
            at("2026-09-10T18:00:00Z"),
        );
        assert_eq!(md["built_by"], "claude@algedonic.dev");
        assert_eq!(md["build_host"], "boss-dev-0");
        assert_eq!(
            md["build_worktree"], "/work/boss/.claude/worktrees/agent-a23",
            "two agents on the same tree is a thing a reader must be able to see"
        );
        assert_eq!(md["build_started_at"], "2026-09-10T18:00:00Z");
        // Absent, never nulled: the metadata door DELETES a null key, so
        // an unknown worktree must not strip one a re-open recorded.
        let thin = build_start("claude@algedonic.dev", "", "", at("2026-09-10T18:00:00Z"));
        assert!(!thin.contains_key("build_host"));
        assert!(!thin.contains_key("build_worktree"));
        assert_eq!(thin["built_by"], "claude@algedonic.dev");
    }

    /// The step id a caller needs for the CLAIM — read off the car, not
    /// assembled from anything.
    #[test]
    fn the_build_step_id_comes_off_the_car() {
        assert_eq!(
            step_id_for(&building("b1", BRANCH, BRANCH), BUILD_SLUG, BUILD).as_deref(),
            Some("s-build")
        );
        assert!(step_id_for(&json!({"steps": []}), BUILD_SLUG, BUILD).is_none());
    }
}

#[cfg(test)]
mod not_yet_streak_tests {
    use super::*;

    const PROBE: &str = "grep -c x f || exit 75";

    fn not_yet(at: &str, since: Option<&str>, runs: Option<u64>) -> Value {
        let mut a = json!({"at": at, "exit": 75, "not_yet": true, "probe": PROBE});
        if let Some(s) = since {
            a[NOT_YET_SINCE] = json!(s);
        }
        if let Some(n) = runs {
            a[NOT_YET_RUNS] = json!(n);
        }
        a
    }

    /// The first not-yet of a streak starts it at this run.
    #[test]
    fn a_first_not_yet_starts_the_streak_at_this_run() {
        let (since, runs) = carried_not_yet_streak(None, PROBE, "2026-09-23T07:00:00Z");
        assert_eq!((since.as_str(), runs), ("2026-09-23T07:00:00Z", 1));
    }

    /// A not-yet after a not-yet of the SAME probe keeps the streak's
    /// start and counts the run — the only way a reader can later tell
    /// "asked once" from "asked 86 times across four days".
    #[test]
    fn a_not_yet_after_a_not_yet_carries_the_start_and_counts() {
        let prior = not_yet(
            "2026-09-23T06:00:00Z",
            Some("2026-09-19T05:50:00Z"),
            Some(85),
        );
        let (since, runs) = carried_not_yet_streak(Some(&prior), PROBE, "2026-09-23T07:00:00Z");
        assert_eq!((since.as_str(), runs), ("2026-09-19T05:50:00Z", 86));
    }

    /// A record written before the streak existed still vouches for its
    /// own run: its `at` is the earliest not-yet this door can prove, so
    /// the streak starts there rather than at zero on every car already
    /// standing in the shed when this lands.
    #[test]
    fn a_legacy_not_yet_record_dates_the_streak_from_its_own_run() {
        let prior = not_yet("2026-09-23T06:00:00Z", None, None);
        let (since, runs) = carried_not_yet_streak(Some(&prior), PROBE, "2026-09-23T07:00:00Z");
        assert_eq!((since.as_str(), runs), ("2026-09-23T06:00:00Z", 2));
    }

    /// THE STREAK BELONGS TO THE PROBE TEXT. A corrected probe is the
    /// repair for a starved one (52e0287e), and inheriting the old
    /// probe's four days would name the repair starved on its first run.
    /// And a run that answered anything but not-yet ends the streak.
    #[test]
    fn a_new_probe_or_a_different_answer_restarts_the_streak() {
        let prior = not_yet(
            "2026-09-23T06:00:00Z",
            Some("2026-09-19T05:50:00Z"),
            Some(85),
        );
        let (since, runs) =
            carried_not_yet_streak(Some(&prior), "a corrected probe", "2026-09-23T07:00:00Z");
        assert_eq!((since.as_str(), runs), ("2026-09-23T07:00:00Z", 1));

        let failed =
            json!({"at": "2026-09-23T06:00:00Z", "exit": 1, "not_yet": false, "probe": PROBE});
        let (since, runs) = carried_not_yet_streak(Some(&failed), PROBE, "2026-09-23T07:00:00Z");
        assert_eq!((since.as_str(), runs), ("2026-09-23T07:00:00Z", 1));
    }

    /// The reader: the streak's length is measured between its first and
    /// its latest not-yet — two runs that happened — never against the
    /// clock, so a recheck that stopped running cannot age a streak.
    #[test]
    fn the_streak_reads_back_as_hours_between_its_first_and_latest_run() {
        let md = json!({
            PROOF_PROBE: PROBE,
            "proof_attempt": not_yet("2026-09-23T07:00:00Z", Some("2026-09-19T05:50:00Z"), Some(86)),
        });
        assert_eq!(
            not_yet_streak(&md),
            Some(NotYetStreak {
                hours: 97,
                runs: 86
            })
        );
        // Legacy: one run vouched for, no length yet.
        let md = json!({
            PROOF_PROBE: PROBE,
            "proof_attempt": not_yet("2026-09-23T07:00:00Z", None, None),
        });
        assert_eq!(
            not_yet_streak(&md),
            Some(NotYetStreak { hours: 0, runs: 1 })
        );
    }

    /// No streak when the last run did not say not-yet, and none when the
    /// car's recorded probe is no longer the one that ran — a probe
    /// corrected by a metadata PATCH stops reading as starved at once,
    /// not an hour later when the recheck next writes.
    #[test]
    fn no_streak_for_another_answer_or_a_since_replaced_probe() {
        let failed = json!({
            PROOF_PROBE: PROBE,
            "proof_attempt": {"at": "2026-09-23T07:00:00Z", "exit": 1, "probe": PROBE},
        });
        assert_eq!(not_yet_streak(&failed), None);
        let replaced = json!({
            PROOF_PROBE: "a corrected probe",
            "proof_attempt": not_yet("2026-09-23T07:00:00Z", Some("2026-09-19T05:50:00Z"), Some(86)),
        });
        assert_eq!(not_yet_streak(&replaced), None);
        assert_eq!(not_yet_streak(&json!({})), None);
    }
}

#[cfg(test)]
mod waits_on_tests {
    use super::*;

    const PROBE: &str = "grep -c x f || exit 75";

    /// A car at `proven` whose probe has said not-yet for `hours` straight.
    fn waiting(hours: i64, waits: Option<Value>, seen_at: Option<&str>) -> Value {
        let last = chrono::DateTime::parse_from_rfc3339("2026-09-26T12:00:00Z").unwrap();
        let since = (last - chrono::Duration::hours(hours)).to_rfc3339();
        let mut attempt = json!({
            "at": last.to_rfc3339(), "exit": 75, "not_yet": true, "probe": PROBE,
            "waits_on_exit": if seen_at.is_some() { 0 } else { 1 },
            "waits_on_observation": if seen_at.is_some() { Value::Null } else { json!("unseen") },
            NOT_YET_SINCE: since, NOT_YET_RUNS: hours + 1,
        });
        if let Some(s) = seen_at {
            attempt[WAITS_ON_SEEN_AT] = json!(s);
        }
        let mut md = json!({PROOF_PROBE: PROBE, "proof_attempt": attempt});
        if let Some(w) = waits {
            md[WAITS_ON] = w;
        }
        md
    }

    /// THE WRITER MERGES, IT DOES NOT REPLACE (backlog e9b164a1 piece 3).
    /// Until this, `boss car waits-on` PATCHed the whole object, so a
    /// re-statement of `on` or `seen` dropped an `owner` an operator had
    /// added by hand — and a wait without an owner reads as ours. Each
    /// field the update carries replaces that field; every other field
    /// the car already declares is kept; a result naming no `on` declares
    /// nothing and is `None`.
    #[test]
    fn a_written_declaration_merges_into_the_one_the_car_carries() {
        let owned = json!({"on": "a release", "seen": "true", WAITS_ON_OWNER: "emp-david"});
        let got = merge_waits_on(
            Some(&owned),
            &json!({"on": "a tagged release", "seen": "exit 0"}),
        );
        assert_eq!(
            got,
            Some(json!({"on": "a tagged release", "seen": "exit 0", WAITS_ON_OWNER: "emp-david"}))
        );
        // An owner and a patience added to a declaration keep its on/seen.
        let got = merge_waits_on(
            Some(&waits_on_value("a Stripe charge", Some("true"))),
            &json!({WAITS_ON_OWNER: "world", WAITS_ON_MAX_WAIT_HOURS: 336}),
        )
        .unwrap();
        let md = json!({ WAITS_ON: got });
        assert_eq!(wait_owner(&md), Some(WaitOwner::World));
        assert_eq!(waits_on(&md).unwrap().seen.as_deref(), Some("true"));
        assert_eq!(md[WAITS_ON][WAITS_ON_MAX_WAIT_HOURS], 336);
        // Nothing to merge into and no `on`: nothing is declared.
        assert_eq!(
            merge_waits_on(None, &json!({WAITS_ON_OWNER: "world"})),
            None
        );
        // A non-object recorded by hand is replaced, not merged into.
        assert_eq!(
            merge_waits_on(Some(&json!("prose only")), &json!({"on": "x"})),
            Some(json!({"on": "x"}))
        );
    }

    /// UNDECLARED: exactly adef5ddf's rule — past the bound it is ours to
    /// read, under it nobody's business yet.
    #[test]
    fn an_undeclared_wait_is_starved_only_past_the_bound() {
        assert!(matches!(
            starved(&waiting(97, None, None)),
            Some(Starved::Undeclared(NotYetStreak { hours: 97, .. }))
        ));
        assert_eq!(starved(&waiting(40, None, None)), None);
    }

    /// DECLARED AND NOT SEEN: the car said what it waits on and the
    /// record does not hold it yet, so however long the streak, the move
    /// is the world's (the six cars b461341d triaged, at 97h to 134h).
    #[test]
    fn a_declared_wait_not_yet_seen_is_never_starved() {
        let w = waits_on_value("a Stripe sponsorship charge", None);
        assert_eq!(starved(&waiting(134, Some(w), None)), None);
    }

    /// DECLARED AND SEEN, PROBE STILL NOT YET: the event the car named
    /// is in the record and the probe still cannot see it — the true
    /// "ours to read", at any streak length.
    #[test]
    fn a_declared_wait_seen_while_the_probe_says_not_yet_is_ours_at_once() {
        let w = waits_on_value("a red crawl", Some("true"));
        let md = waiting(3, Some(w), Some("2026-09-26T11:00:00Z"));
        assert_eq!(
            starved(&md),
            Some(Starved::SeenWhileNotYet {
                on: "a red crawl".into(),
                seen_at: "2026-09-26T11:00:00Z".into(),
            })
        );
    }

    /// A declaration must name something: a blank `on` is no declaration,
    /// so it cannot silence the label by being present.
    #[test]
    fn a_blank_or_malformed_declaration_is_no_declaration() {
        assert_eq!(waits_on(&json!({WAITS_ON: {"on": "  "}})), None);
        assert_eq!(waits_on(&json!({WAITS_ON: "prose only"})), None);
        assert!(starved(&waiting(97, Some(json!({"on": ""})), None)).is_some());
        assert_eq!(
            waits_on(&json!({WAITS_ON: waits_on_value("x", Some(" "))})),
            Some(WaitsOn {
                on: "x".into(),
                seen: None
            })
        );
    }

    /// The first run that saw the event dates the sighting; later runs
    /// that still see it keep that date, and a run that does not see it
    /// clears it.
    #[test]
    fn a_sighting_is_dated_from_the_first_run_that_saw_it() {
        let prior = json!({WAITS_ON_SEEN_AT: "2026-09-26T09:00:00Z"});
        assert_eq!(
            carried_seen_at(Some(&prior), true, "2026-09-26T10:00:00Z").as_deref(),
            Some("2026-09-26T09:00:00Z")
        );
        assert_eq!(
            carried_seen_at(None, true, "2026-09-26T10:00:00Z").as_deref(),
            Some("2026-09-26T10:00:00Z")
        );
        assert_eq!(carried_seen_at(Some(&prior), false, "x"), None);
    }

    /// A declared wait with its owner and optional patience added.
    fn owned(on: &str, seen: Option<&str>, owner: Value, max: Value) -> Value {
        let mut w = waits_on_value(on, seen);
        w[WAITS_ON_OWNER] = owner;
        w[WAITS_ON_MAX_WAIT_HOURS] = max;
        w
    }

    /// THE OWNER IS READ, NEVER INFERRED (backlog 3881f5c9): `world`
    /// (any case) is the world, any other non-blank string is the actor
    /// named, and a blank or missing owner is no owner — the `on` prose
    /// saying "David opens it" does not make David the owner.
    #[test]
    fn a_wait_owner_is_the_declared_field_and_nothing_else() {
        let md =
            |owner: Value| json!({WAITS_ON: owned("an event", Some("true"), owner, Value::Null)});
        assert_eq!(wait_owner(&md(json!(" World "))), Some(WaitOwner::World));
        assert_eq!(
            wait_owner(&md(json!("emp-david"))),
            Some(WaitOwner::Actor("emp-david".into()))
        );
        assert_eq!(wait_owner(&md(json!("  "))), None);
        assert_eq!(wait_owner(&md(Value::Null)), None);
        let prose = json!({WAITS_ON: waits_on_value("a release (David opens it)", Some("true"))});
        assert_eq!(wait_owner(&prose), None);
    }

    /// OBSERVED, OWNED, NOT SEEN: someone else's move, and only that.
    /// Every one of the three missing — the owner, the `seen` check, or
    /// the event still unseen — leaves the wait ours.
    #[test]
    fn an_owned_wait_is_only_a_declared_observed_owned_unseen_one() {
        let w = owned("a Stripe charge", Some("true"), json!("world"), json!(336));
        let md = waiting(134, Some(w.clone()), None);
        assert_eq!(
            owned_wait(&md),
            Some(OwnedWait {
                owner: WaitOwner::World,
                on: "a Stripe charge".into(),
                max_wait_hours: Some(336),
            })
        );
        // Seen while not yet: ours, not the owner's.
        assert_eq!(
            owned_wait(&waiting(3, Some(w), Some("2026-09-26T11:00:00Z"))),
            None
        );
        // No `seen` check: nothing can say the event came.
        let unobserved = owned("a Stripe charge", None, json!("world"), Value::Null);
        assert_eq!(owned_wait(&waiting(134, Some(unobserved), None)), None);
        // No owner.
        let unowned = waits_on_value("a Stripe charge", Some("true"));
        assert_eq!(owned_wait(&waiting(134, Some(unowned), None)), None);
        // Undeclared.
        assert_eq!(owned_wait(&waiting(134, None, None)), None);
    }

    /// A declared observer that explicitly did not run is not evidence
    /// that the external event remains unseen.
    #[test]
    fn an_unread_observer_cannot_qualify_an_owned_world_wait() {
        let w = owned("a Stripe charge", Some("true"), json!("world"), json!(336));
        let mut md = waiting(3, Some(w), None);
        md["proof_attempt"]["waits_on_exit"] = Value::Null;
        assert_eq!(owned_wait(&md), None);
        md["proof_attempt"]["waits_on_exit"] = json!(1);
        assert!(
            owned_wait(&md).is_some(),
            "an executed negative observation remains a wait"
        );
    }

    #[test]
    fn a_world_wait_requires_an_actual_integer_observer_outcome() {
        for outcome in [None, Some(Value::Null), Some(json!("1")), Some(json!(true))] {
            let w = owned("a Stripe charge", Some("false"), json!("world"), json!(336));
            let mut md = waiting(3, Some(w), None);
            let attempt = md["proof_attempt"].as_object_mut().unwrap();
            attempt.remove("waits_on_exit");
            if let Some(value) = outcome {
                attempt.insert("waits_on_exit".into(), value);
            }
            assert_eq!(
                owned_wait(&md),
                None,
                "unread observer is not an observed wait: {md}"
            );
        }
        let w = owned("a Stripe charge", Some("false"), json!("world"), json!(336));
        let mut md = waiting(3, Some(w), None);
        md["proof_attempt"]["waits_on_exit"] = json!(1);
        assert!(owned_wait(&md).is_some());
    }

    /// A NAMED ACTOR'S ACT NEEDS NO SEEN CHECK TO BE THEIRS (backlog
    /// 3881f5c9, fix shape (2)). Measured 2026-09-24 16:42Z: the shed
    /// read troubled on ONE car, the dev-door login, whose declared wait
    /// is David's Access SSH CA ceremony — `owner: emp-david`, no
    /// `seen`, no probe. The observation rule exists because nothing
    /// else can say a WORLD event arrived; an actor's act has its actor,
    /// whose next move it is, and the car's own declared patience still
    /// bounds it. A world wait with no `seen` stays ours, and an actor's
    /// wait whose check DID see the act while the probe says not yet is
    /// ours too — the declaration never hides a contradiction.
    #[test]
    fn a_named_actors_act_is_theirs_without_a_seen_check_and_the_worlds_is_not() {
        let act = owned("the SSH CA ceremony", None, json!("emp-david"), Value::Null);
        assert_eq!(
            owned_wait(&json!({ WAITS_ON: act })),
            Some(OwnedWait {
                owner: WaitOwner::Actor("emp-david".into()),
                on: "the SSH CA ceremony".into(),
                max_wait_hours: None,
            }),
            "declared on no probe at all, as the dev-door car is"
        );
        let world = owned("a Stripe charge", None, json!("world"), Value::Null);
        assert_eq!(owned_wait(&json!({ WAITS_ON: world })), None);
        let seen = owned("a release", Some("true"), json!("emp-david"), Value::Null);
        assert_eq!(
            owned_wait(&waiting(3, Some(seen), Some("2026-09-26T11:00:00Z"))),
            None
        );
    }

    /// PATIENCE IS OPTIONAL AND BOUNDED ONLY WHEN DECLARED: no
    /// `max_wait_hours` is never overdue; a declared one is overdue past
    /// it on the car's age; a zero, negative or non-numeric one is no
    /// declaration, so it cannot turn a car overdue the hour it lands.
    #[test]
    fn a_declared_max_wait_bounds_the_owned_wait_and_nothing_else_does() {
        let read = |max: Value| {
            let w = owned("a release", Some("true"), json!("emp-david"), max);
            owned_wait(&waiting(10, Some(w), None)).unwrap()
        };
        assert!(!read(Value::Null).overdue(10_000));
        assert!(read(json!(48)).overdue(49));
        assert!(!read(json!(48)).overdue(48));
        assert_eq!(read(json!(0)).max_wait_hours, None);
        assert_eq!(read(json!(-5)).max_wait_hours, None);
        assert_eq!(read(json!("48")).max_wait_hours, None);
    }

    /// A declared wait with the event a machine can match — the close of
    /// a packet of one kind, optionally narrowed by its title.
    fn with_event(event: Value) -> Value {
        let mut w = waits_on_value("a cut-a-release tag", Some("true"));
        w[WAITS_ON_EVENT] = event;
        json!({ WAITS_ON: w })
    }

    fn close_marker(kind: &str, title: &str) -> Value {
        json!({"id": "p1", "kind": kind, "title": title, "outcome": "answered",
               "closed_on": "2026-09-24", "subject_id": "forge", "parent_step_id": null})
    }

    /// THE EVENT IS DECLARED AS DATA, and read back only when it names
    /// the kind whose close it is. A blank or absent `closes`, a wait
    /// with no `on`, or no `event` at all is no declaration — so an
    /// empty object cannot key an obligation to every close there is.
    #[test]
    fn a_wait_event_is_read_only_when_it_names_the_closing_kind() {
        let md = with_event(json!({"closes": "ops-request", "title": "tag-release"}));
        assert_eq!(
            wait_event(&md),
            Some(WaitEvent {
                closes: "ops-request".into(),
                title: Some("tag-release".into()),
            })
        );
        let bare = with_event(json!({"closes": "maintenance-playground-crawl"}));
        assert_eq!(wait_event(&bare).and_then(|e| e.title), None);
        assert_eq!(wait_event(&with_event(json!({"closes": "  "}))), None);
        assert_eq!(wait_event(&with_event(json!({}))), None);
        assert_eq!(wait_event(&with_event(Value::Null)), None);
        let no_on = json!({ WAITS_ON: {"on": "", "event": {"closes": "ops-request"}} });
        assert_eq!(wait_event(&no_on), None);
        assert_eq!(wait_event(&json!({})), None);
    }

    /// FIRED BY exactly the close it names: the kind must match, and a
    /// declared title prefix must lead the closing packet's title. A
    /// marker with no title never satisfies a declared prefix.
    #[test]
    fn a_wait_event_is_fired_by_the_close_it_names_and_no_other() {
        let tag = wait_event(&with_event(
            json!({"closes": "ops-request", "title": "tag-release"}),
        ))
        .unwrap();
        assert!(tag.fired_by(&close_marker(
            "ops-request",
            "tag-release on forge — cut-a-release v0.4.0"
        )));
        assert!(!tag.fired_by(&close_marker(
            "ops-request",
            "converge on forge — a train merged"
        )));
        assert!(!tag.fired_by(&close_marker("cut-a-release", "tag-release v0.4.0")));
        assert!(!tag.fired_by(&json!({"kind": "ops-request", "title": null})));
        let crawl = wait_event(&with_event(
            json!({"closes": "maintenance-playground-crawl"}),
        ))
        .unwrap();
        assert!(crawl.fired_by(&close_marker("maintenance-playground-crawl", "anything")));
        assert!(!crawl.fired_by(&close_marker("maintenance-sweep", "anything")));
    }

    /// OWED IS ONE MARKER, spelled once: the string `"true"`. Anything
    /// else — absent, a boolean, `"paid"` — owes nothing, so a car whose
    /// proof was paid drops out of the obligation by the same read.
    #[test]
    fn a_car_owes_its_proof_only_under_the_one_marker() {
        assert!(owes_proof(&json!({ PROOF_OWED: "true" })));
        assert!(!owes_proof(&json!({ PROOF_OWED: true })));
        assert!(!owes_proof(&json!({ PROOF_OWED: "paid" })));
        assert!(!owes_proof(&json!({})));
    }

    /// THE PAYMENT IS THE RUNNER'S (backlog 82946cd9): every move of the
    /// two keys but one — marking a debt the row does not hold — is a
    /// move only the credentialed runner may make; `null` is absent on
    /// both sides, and an unchanged row moves nothing.
    #[test]
    fn every_move_of_the_payment_but_marking_a_debt_is_the_runners() {
        let owed = json!({ PROOF_OWED: "true" });
        let paid = json!({ PROOF_OWED: "paid", OWED_PROOF: {"verified": "v"} });
        assert!(
            payment_moves(&json!({}), &owed).is_empty(),
            "marking a debt"
        );
        assert!(payment_moves(&json!({ PROOF_OWED: null }), &owed).is_empty());
        assert!(payment_moves(&paid, &paid).is_empty(), "an unchanged row");
        assert!(payment_moves(&json!({}), &json!({ OWED_PROOF: null })).is_empty());
        assert_eq!(payment_moves(&owed, &paid), [OWED_PROOF, PROOF_OWED]);
        assert_eq!(
            payment_moves(&owed, &json!({})),
            [PROOF_OWED],
            "erasing a debt"
        );
        assert_eq!(
            payment_moves(&json!({}), &json!({ PROOF_OWED: "paid" })),
            [PROOF_OWED]
        );
        assert_eq!(
            payment_moves(&paid, &owed),
            [OWED_PROOF, PROOF_OWED],
            "re-owing"
        );
        let other = json!({ PROOF_OWED: "paid", OWED_PROOF: {"verified": "w"} });
        assert_eq!(
            payment_moves(&paid, &other),
            [OWED_PROOF],
            "a rewritten proof"
        );
    }

    /// Only a credential the server resolved for `runner:ops` pays, and
    /// only from a host the run-car-probe verb runs on (the host binding
    /// still owed on 82946cd9): the boss-gcp runner holds the same
    /// principal and runs no probe, and a host-less runner names no host
    /// to judge. No credential, or another principal's, is refused with
    /// the reason the refusal carries.
    #[test]
    fn only_the_runner_credential_pays_and_only_from_a_probe_host() {
        use crate::field_writer::CredentialedCaller;
        let runner = |host: Option<&str>| CredentialedCaller {
            principal: crate::runner_credential::PRINCIPAL.into(),
            actor_id: crate::runner_credential::ACTOR.into(),
            host: host.map(str::to_string),
        };
        assert_eq!(payment_refusal(Some(&runner(Some("forge")))), None);
        for elsewhere in [Some("boss-gcp"), None] {
            let why = payment_refusal(Some(&runner(elsewhere))).unwrap_or_default();
            assert!(
                why.contains("run-car-probe") && why.contains("`forge`"),
                "{elsewhere:?}: {why}"
            );
        }
        let none = payment_refusal(None).unwrap_or_default();
        assert!(none.contains("self-asserted"), "{none}");
        let other = CredentialedCaller {
            principal: "runner:other".into(),
            ..runner(Some("forge"))
        };
        assert!(payment_refusal(Some(&other)).is_some());
    }

    /// The hosts a payment may come from are READ from the verb that
    /// pays — infra/ops/verbs/run-car-probe.json's `hosts` — never a
    /// copy (CLAUDE.md §9a); a verb file that stopped parsing would
    /// answer no host and refuse every payment, so it is pinned here.
    #[test]
    fn the_payment_hosts_are_the_run_car_probe_verbs_own() {
        assert_eq!(payment_hosts(), ["forge"]);
    }
}

// ---------------------------------------------------------------------------
// The declared ordering edge — the four refusals, and the fail-open.
// ---------------------------------------------------------------------------

/// WHAT AN OPERATOR DEPENDS ON HERE IS THE WORDING, so the wording is
/// what these assert. David on d3320278: *"the refusal's wording matters
/// as much as its existence: the dock's no-departure line is read by an
/// operator deciding whether the pipeline is stuck, so 'held: boards
/// after <car>, which is abandoned' has to be distinguishable from
/// 'held: boards after <car>, still in flight' — the first needs a
/// human, the second does not."*
///
/// A test that only checked "it held" would let the four collapse into
/// one message a release later, which is the quiet hold the feature
/// exists to remove.
///
/// Moved from `boss-cli/src/train/boarding.rs` with the function they
/// pin (backlog 4142d821): the words are unchanged, and so are these.
#[cfg(test)]
mod boards_after_tests {
    use super::{
        EDGE_HOLD_NEEDS_HUMAN, EDGE_HOLD_WAITING, EdgeOutcome, Predecessor, UNSCOPED_PREDECESSOR,
        boards_after_of, boards_after_outcome, declared_predecessor,
    };
    use serde_json::{Value, json};

    const PRED: &str = "bbbbbbbb-1111-2222-3333-444444444444";

    /// A predecessor packet: open, with a branch, and whatever extra
    /// metadata / steps the situation needs.
    fn pred(status: &str, md: Value, steps: Value) -> Value {
        let mut metadata = json!({"branch": "fix/the-predecessor"});
        if let (Some(dst), Some(src)) = (metadata.as_object_mut(), md.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        json!({"id": PRED, "status": status, "metadata": metadata, "steps": steps})
    }

    fn review(status: &str) -> Value {
        json!([{"spec_slug": "review", "status": status}])
    }

    fn hold_reason(declared: &str, p: &Predecessor) -> String {
        match boards_after_outcome(declared, p) {
            EdgeOutcome::Hold(h) => h.reason,
            other => panic!("expected a hold, got {other:?}"),
        }
    }

    /// (1) STILL IN FLIGHT — nobody needs to do anything, and the line
    /// says so outright. It also names WHERE the predecessor is, because
    /// "in flight" alone sends the reader to the yard to find out.
    #[test]
    fn a_predecessor_in_flight_holds_and_asks_for_nobody() {
        let p = Predecessor::Found(pred(
            "open",
            json!({"train": "77777777-aaaa-bbbb-cccc-dddddddddddd"}),
            review("ready"),
        ));
        let r = hold_reason(PRED, &p);
        assert!(
            r.contains("STILL IN FLIGHT") && r.contains("aboard train 77777777"),
            "it must name the state AND where: {r}"
        );
        assert!(
            r.contains("no action needed"),
            "an operator deciding whether the pipeline is stuck must be told it is not: {r}"
        );
        assert!(
            !r.contains("human"),
            "a self-clearing hold must never read as one that needs a person: {r}"
        );
        assert_eq!(
            match boards_after_outcome(PRED, &p) {
                EdgeOutcome::Hold(h) => h.kind,
                other => panic!("{other:?}"),
            },
            EDGE_HOLD_WAITING
        );
    }

    /// The dock and the build are in-flight states too, and each names
    /// itself — a car waiting on one still building is a different wait
    /// from one waiting on a car about to merge.
    #[test]
    fn in_flight_names_the_dock_and_the_build_separately() {
        let parked = hold_reason(
            PRED,
            &Predecessor::Found(pred("open", json!({}), review("ready"))),
        );
        assert!(parked.contains("parked at the dock"), "{parked}");
        let building = hold_reason(
            PRED,
            &Predecessor::Found(pred(
                "open",
                json!({}),
                json!([{"spec_slug": "gate", "status": "ready"}]),
            )),
        );
        assert!(building.contains("still building"), "{building}");
    }

    /// (2) LANDED — the edge is satisfied and the car boards. If this
    /// ever holds, the bug is in the filter and not on the dock.
    #[test]
    fn a_landed_predecessor_satisfies_the_edge() {
        for landed in [
            pred("closed", json!({"outcome": "merged"}), json!([])),
            pred("open", json!({"merged": "true"}), review("ready")),
        ] {
            assert_eq!(
                boards_after_outcome(PRED, &Predecessor::Found(landed.clone())),
                EdgeOutcome::Board,
                "a landed predecessor must board its successor: {landed}"
            );
        }
    }

    /// (3) ABANDONED — it can NEVER clear, so the line says a human must
    /// act, says what to do, and reports how the record says it ended.
    #[test]
    fn an_abandoned_predecessor_names_a_human_and_what_to_clear() {
        let p = Predecessor::Found(pred(
            "closed",
            json!({"outcome": "abandoned"}),
            review("ready"),
        ));
        let r = hold_reason(PRED, &p);
        assert!(r.contains("ABANDONED"), "{r}");
        assert!(
            r.contains("can never be satisfied"),
            "waiting is futile and the line must say so: {r}"
        );
        assert!(
            r.contains("a human must clear metadata.boards_after"),
            "name the fix, not just the fault: {r}"
        );
        assert!(
            r.contains("closed, outcome 'abandoned'"),
            "report what the record says, not a guess: {r}"
        );
        assert!(
            !r.contains("no action needed"),
            "this one DOES need action: {r}"
        );
        assert_eq!(
            match boards_after_outcome(PRED, &p) {
                EdgeOutcome::Hold(h) => h.kind,
                other => panic!("{other:?}"),
            },
            EDGE_HOLD_NEEDS_HUMAN
        );
    }

    /// A cancelled predecessor is spent, not landed — the same refusal,
    /// and it must not be read as in flight just because `outcome` is
    /// missing.
    #[test]
    fn a_cancelled_predecessor_is_spent_not_in_flight() {
        let r = hold_reason(
            PRED,
            &Predecessor::Found(pred("cancelled", json!({}), review("ready"))),
        );
        assert!(
            r.contains("ABANDONED") && r.contains("cancelled, no landing recorded"),
            "{r}"
        );
    }

    /// (4) NO SUCH JOB — a human must fix the REFERENCE, which is a
    /// different repair from breaking a live edge, so it gets different
    /// words. The line also says this should have been impossible, so the
    /// reader knows to suspect the write path and not the car.
    #[test]
    fn a_dangling_edge_says_the_job_does_not_exist() {
        let r = hold_reason(PRED, &Predecessor::Absent);
        assert!(r.contains("DOES NOT EXIST"), "{r}");
        assert!(
            r.contains("a human must fix metadata.boards_after"),
            "fix the reference, do not break the edge: {r}"
        );
        assert!(r.contains("ref-checked"), "say why this is surprising: {r}");
    }

    /// A PREDECESSOR OUTSIDE THE READER'S SCOPE (backlog 0964ba80) is
    /// judged like any other — landed boards, anything else holds — but
    /// in flight and abandoned read IDENTICALLY, and neither names the
    /// branch, the id or where it stands: telling them apart would hand
    /// the reader the state its scope does not read.
    #[test]
    fn a_predecessor_outside_the_scope_holds_without_being_described() {
        let landed = boards_after_outcome(
            PRED,
            &Predecessor::Unscoped(pred("closed", json!({"outcome": "merged"}), json!([]))),
        );
        assert_eq!(landed, EdgeOutcome::Board);
        let in_flight = boards_after_outcome(
            PRED,
            &Predecessor::Unscoped(pred("open", json!({}), review("ready"))),
        );
        let abandoned = boards_after_outcome(
            PRED,
            &Predecessor::Unscoped(pred("closed", json!({"outcome": "abandoned"}), json!([]))),
        );
        assert_eq!(in_flight, abandoned, "the two states read alike");
        let EdgeOutcome::Hold(h) = in_flight else {
            panic!("an unlanded predecessor holds the car: {in_flight:?}");
        };
        assert_eq!(h.kind, EDGE_HOLD_WAITING);
        assert_eq!(h.behind, UNSCOPED_PREDECESSOR);
        for leak in [
            "fix/the-predecessor",
            &PRED[..8],
            "IN FLIGHT",
            "ABANDONED",
            "parked",
        ] {
            assert!(!h.reason.contains(leak), "{leak} in: {}", h.reason);
        }
        assert!(
            h.reason.contains("outside this caller's policy scope"),
            "{}",
            h.reason
        );

        // The control: the same packet read IN scope is described.
        let named = hold_reason(
            PRED,
            &Predecessor::Found(pred("open", json!({}), review("ready"))),
        );
        assert!(named.contains("fix/the-predecessor"), "{named}");
    }

    /// THE ASSERTION THE FEATURE IS TRUSTED ON: no two of the four read
    /// the same, and each side of the needs-a-human line is recognisable
    /// without reading the whole sentence.
    #[test]
    fn the_four_situations_are_told_apart_by_their_words() {
        let in_flight = hold_reason(
            PRED,
            &Predecessor::Found(pred("open", json!({}), review("ready"))),
        );
        let abandoned = hold_reason(
            PRED,
            &Predecessor::Found(pred("closed", json!({"outcome": "abandoned"}), json!([]))),
        );
        let absent = hold_reason(PRED, &Predecessor::Absent);
        let unjudged = match boards_after_outcome(PRED, &Predecessor::Unreadable("boom".into())) {
            EdgeOutcome::BoardUnjudged(note) => note,
            other => panic!("an unreadable edge must still board: {other:?}"),
        };
        let landed = boards_after_outcome(
            PRED,
            &Predecessor::Found(pred("closed", json!({"outcome": "merged"}), json!([]))),
        );

        let all = [&in_flight, &abandoned, &absent, &unjudged];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "two situations read identically");
            }
        }
        assert_eq!(landed, EdgeOutcome::Board, "landed is not a refusal at all");
        // The one-glance test: does this need a person?
        assert!(!in_flight.contains("human") && in_flight.contains("no action needed"));
        assert!(abandoned.contains("a human must") && !abandoned.contains("no action needed"));
        assert!(absent.contains("a human must") && !absent.contains("no action needed"));
        assert!(unjudged.contains("boarding anyway"));
    }

    /// THE HAZARD THIS CAR WAS WARNED ABOUT. A read failure must not
    /// hold the dock: the conductor boards the car it cannot judge and
    /// says why, loudly. Refusing everything it could not evaluate would
    /// freeze every landing, and the gate does not run the conductor.
    #[test]
    fn an_unreadable_edge_boards_the_car_and_says_why() {
        let note = match boards_after_outcome(
            PRED,
            &Predecessor::Unreadable("HTTP 503 Service Unavailable".into()),
        ) {
            EdgeOutcome::BoardUnjudged(n) => n,
            other => panic!("fail-open is the whole point: {other:?}"),
        };
        assert!(note.contains("HTTP 503"), "carry the cause: {note}");
        assert!(note.contains("boarding anyway"), "{note}");
        assert!(
            note.contains("would stop every train"),
            "say why fail-open is the right choice here: {note}"
        );
    }

    /// THE REGRESSION THAT MATTERS MOST: every car in flight today has
    /// no edge, and must behave exactly as it did before this car.
    #[test]
    fn a_car_with_no_edge_declares_no_predecessor() {
        for md in [
            json!({"branch": "fix/x"}),
            json!({"branch": "fix/x", "boards_after": ""}),
            json!({"branch": "fix/x", "boards_after": "   "}),
            json!({"branch": "fix/x", "boards_after": Value::Null}),
        ] {
            let car = json!({"id": "c", "status": "open", "metadata": md});
            assert_eq!(
                declared_predecessor(&car),
                None,
                "no edge, or a cleared one, is not a constraint: {car}"
            );
        }
        let declared = json!({"id": "c", "metadata": {"boards_after": PRED}});
        assert_eq!(declared_predecessor(&declared).as_deref(), Some(PRED));
        assert_eq!(
            boards_after_of(&json!({"boards_after": PRED})).as_deref(),
            Some(PRED),
            "the dock reads the edge off a Job's metadata, the conductor off the packet — \
             one reading of one key"
        );
    }

    /// THE DOCK'S HALF (backlog 4142d821): a hold names what it waits
    /// behind as DATA, so the dock region can say "waiting behind X"
    /// without parsing the reason sentence back apart.
    #[test]
    fn a_hold_names_what_it_waits_behind() {
        let in_flight = Predecessor::Found(pred("open", json!({}), review("ready")));
        match boards_after_outcome(PRED, &in_flight) {
            EdgeOutcome::Hold(h) => {
                assert_eq!(h.behind, "fix/the-predecessor (car bbbbbbbb)");
                assert!(h.reason.contains(&h.behind), "{}", h.reason);
            }
            other => panic!("{other:?}"),
        }
        match boards_after_outcome(PRED, &Predecessor::Absent) {
            EdgeOutcome::Hold(h) => assert_eq!(h.behind, "car bbbbbbbb", "no packet, no branch"),
            other => panic!("{other:?}"),
        }
    }
}

#[cfg(test)]
mod ending_tests {
    //! A car admitted at build whose run or item ends before any gate
    //! (backlog 366c9fb1; Codex review of car 0d04ee3e, R4a and R4b).
    use super::*;

    const RUN: &str = "run-1";
    const ITEM: &str = "item-1";

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-11-09T08:00:00Z")
            .unwrap()
            .into()
    }

    /// An admitted car at build, carrying the `abandoned` terminal its
    /// pinned version declares.
    fn admitted() -> Value {
        json!({
            "id": "a1",
            "kind": "ship-a-change",
            "status": "open",
            "metadata": {"branch": "fix/x", "agent_run": RUN, BACKLOG_ITEM: ITEM,
                         ADMITTED_BY: "boss dispatch --started"},
            "steps": [
                {"id": "s-opened", "spec_slug": "opened", "title": OPENED, "status": "completed"},
                {"id": "s-scope", "spec_slug": "scope", "title": SCOPE, "status": "completed"},
                {"id": "s-build", "spec_slug": "build", "title": BUILD, "status": "active"},
                {"id": "s-gate", "spec_slug": "gate", "title": GATE, "status": "pending"},
                {"id": "s-review", "spec_slug": "review", "title": REVIEW, "status": "pending"},
                {"id": "s-abandoned", "spec_slug": crate::car_retire::ABANDONED_SLUG,
                 "title": crate::car_retire::ABANDONED, "status": "pending"},
            ],
        })
    }

    fn refused(run: &str) -> Ended<'_> {
        Ended::Run {
            run,
            outcome: "refused",
        }
    }

    /// The claim lands on the gate step FIRST, judged on no gate-run, no
    /// ending and the run binding it read — the same key a launch's own
    /// first write is fenced on.
    #[test]
    fn an_unlaunched_car_is_claimed_on_its_gate_step_first() {
        let claim = ending_claim(&admitted(), refused(RUN), now()).unwrap();
        assert_eq!(claim.method, "PATCH");
        assert_eq!(claim.path, "/api/jobs/a1/steps/s-gate/metadata");
        assert_eq!(claim.body[ENDED_BEFORE_GATE]["by"], "run");
        assert_eq!(claim.body[ENDED_BEFORE_GATE]["id"], RUN);
        assert_eq!(claim.body[ENDED_BEFORE_GATE]["outcome"], "refused");
        assert_eq!(
            Value::Object(claim.expect),
            json!({GATE_RUN: null, ENDED_BEFORE_GATE: null, BUILD_RUN: null})
        );
        // A gate launch is fenced on the same key being absent.
        assert_eq!(
            Value::Object(launch_fence()),
            json!({ENDED_BEFORE_GATE: null})
        );
        let plan = gate_launch(&admitted(), "gr-1", "abc", "t", now())
            .unwrap()
            .unwrap();
        assert_eq!(plan.fence, launch_fence());
    }

    /// R4a: what is owed is read off the claim and the car, so a retry
    /// after ANY write finishes the rest and sends no write twice that
    /// already landed — and a finished ending owes nothing.
    #[test]
    fn an_ending_resumes_from_whatever_write_it_last_landed() {
        let mut c = admitted();
        let claim = ending_claim(&c, refused(RUN), now()).unwrap();
        let claimed = claim.body[ENDED_BEFORE_GATE].clone();
        c["steps"][3]["metadata"] = claim.body.clone();
        assert_eq!(ending_of(&c), Some(&claimed));
        // Claimed, a second claim is refused: the ending is resumed instead.
        assert!(ending_claim(&c, refused(RUN), now()).is_err());

        let all = ending_rest(&c).unwrap();
        let paths: Vec<_> = all.iter().map(|w| (w.method, w.path.as_str())).collect();
        assert_eq!(
            paths,
            vec![
                ("PATCH", "/api/jobs/a1/steps/s-abandoned/metadata"),
                ("PATCH", "/api/jobs/a1/metadata"),
                ("PUT", "/api/jobs/a1/steps/s-abandoned"),
            ]
        );
        let why = all[1].body[ABANDONED_WHY].as_str().unwrap().to_string();
        assert!(why.contains(RUN) && why.contains("refused"), "{why}");
        assert_eq!(all[0].body["evidence"], why.as_str());
        assert_eq!(all[0].body["completed_at"], claimed["at"]);
        assert_eq!(all[1].body[crate::car_retire::ABANDONED_MARKER], "true");

        // After the evidence: the marker and the status are owed.
        c["steps"][5]["metadata"] = all[0].body.clone();
        assert_eq!(ending_rest(&c).unwrap(), all[1..]);
        // After the marker: the status alone.
        c["metadata"][crate::car_retire::ABANDONED_MARKER] = json!("true");
        c["metadata"][ABANDONED_WHY] = json!(why);
        assert_eq!(ending_rest(&c).unwrap(), all[2..]);
        // Finished: nothing.
        c["steps"][5]["status"] = json!("completed");
        assert!(ending_rest(&c).unwrap().is_empty());
    }

    /// The declined item ends the car building for it.
    #[test]
    fn a_declined_item_claims_the_car_building_for_it() {
        let claim = ending_claim(
            &admitted(),
            Ended::Item {
                item: ITEM,
                outcome: "declined",
            },
            now(),
        )
        .unwrap();
        assert_eq!(claim.body[ENDED_BEFORE_GATE]["by"], "item");
        let mut c = admitted();
        c["steps"][3]["metadata"] = claim.body;
        let why = ending_rest(&c).unwrap()[1].body[ABANDONED_WHY].clone();
        assert!(
            why.as_str().unwrap().contains("backlog item item-1"),
            "{why}"
        );
    }

    /// What an ending leaves alone, each named.
    #[test]
    fn an_ending_leaves_a_gated_adopted_or_foreign_car_alone() {
        // A repair adopted it: the car's edge names the repair now.
        let mut adopted = admitted();
        adopted["metadata"]["agent_run"] = json!("run-2");
        let e = ending_claim(&adopted, refused(RUN), now()).unwrap_err();
        assert!(e.contains("no longer names run-1"), "{e}");
        // A repair bound the gate step, its job edge not yet written.
        let mut bound = admitted();
        bound["steps"][3]["metadata"] = json!({BUILD_RUN: "run-2"});
        let e = ending_claim(&bound, refused(RUN), now()).unwrap_err();
        assert!(e.contains("bound to run run-2"), "{e}");
        // ...but its own binding is no bar, and is expected as read.
        bound["steps"][3]["metadata"] = json!({BUILD_RUN: RUN});
        let claim = ending_claim(&bound, refused(RUN), now()).unwrap();
        assert_eq!(claim.expect[BUILD_RUN], RUN);
        // A launch's fence landed: the gate-run holds the car.
        let mut gated = admitted();
        gated["steps"][3]["metadata"] = json!({GATE_RUN: "gr-1"});
        let e = ending_claim(&gated, refused(RUN), now()).unwrap_err();
        assert!(e.contains("gr-1"), "{e}");
        // Build handed in.
        let mut handed = admitted();
        handed["steps"][2]["status"] = json!("completed");
        assert!(ending_claim(&handed, refused(RUN), now()).is_err());
        // Another item's car.
        let other = Ended::Item {
            item: "item-9",
            outcome: "declined",
        };
        assert!(ending_claim(&admitted(), other, now()).is_err());
        // No `abandoned` terminal on its pinned version.
        let mut old = admitted();
        old["steps"].as_array_mut().unwrap().pop();
        assert!(ending_claim(&old, refused(RUN), now()).is_err());
    }

    /// A repair's adoption binds its run on the gate step, landing only
    /// while no ending has claimed the car.
    #[test]
    fn a_repairs_adoption_is_fenced_on_no_ending() {
        let (step, body, expect) = adoption(&admitted(), "run-2").unwrap();
        assert_eq!(step, "s-gate");
        assert_eq!(body, json!({BUILD_RUN: "run-2"}));
        assert_eq!(Value::Object(expect), json!({ENDED_BEFORE_GATE: null}));
    }

    /// A green that read an unbound gate is fenced on no ending too: a
    /// claimed car is the ending's, not the green's to finish.
    #[test]
    fn an_unbound_greens_gate_writes_expect_no_ending() {
        assert!(unbound_gate().contains_key(ENDED_BEFORE_GATE));
        assert_eq!(unbound_gate()[ENDED_BEFORE_GATE], Value::Null);
    }
}
