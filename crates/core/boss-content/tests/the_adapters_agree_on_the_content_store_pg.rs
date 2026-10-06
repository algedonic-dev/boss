//! The content store answers the same on both `ContentRepository`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree" (`boss_testing::adapters_agree!`), on the next core port in
//! the census (backlog be459ab9): `ContentRepository`, which
//! `InMemoryContent` and `PgContent` implement.
//!
//! WHY THIS PORT. It holds the HR bulletin board every My Day renders
//! and the company manual with its append-only version history. The
//! content door's own unit tests (`boss-content/src/http.rs`) ask
//! `InMemoryContent`; production is answered by `PgContent`, and the
//! double's header promises only "fast integration tests" — nothing
//! held the two to one contract until this file.
//!
//! The first run found four disagreements, fixed in this car in
//! production code:
//!
//! - ADMIN LIST ORDER. `list_all_bulletins` sorted by `posted_on` alone
//!   in the double and by `posted_on, created_at` in Postgres, and
//!   neither broke a tie on both — two bulletins posted the same day
//!   answered in HashMap order in one and plan order in the other. Both
//!   now answer posted-desc, created-desc, then id. Case
//!   `the_admin_list_is_every_bulletin_by_posted_then_created_then_id`.
//! - BOARD TIE. `list_bulletins_for` sorted priority, posted, created on
//!   both and stopped there, so two bulletins sharing all three
//!   answered in whatever order each store held them. Both now end on
//!   id. Case
//!   `the_board_is_the_viewers_live_rows_by_priority_posted_created_then_id`.
//! - TREE ORDER. `manual_tree` promises "(parent_slug, sort_order,
//!   title)"; Postgres sorted both texts by the database's locale,
//!   which ignores `-` at first level and folds case, and the double by
//!   `String::cmp` (backlog 2987fb2d's class), and neither broke a tie
//!   on a shared title. Postgres now sorts `COLLATE "C"`, and both end
//!   on the unique slug. Case
//!   `the_tree_is_by_parent_then_sort_order_then_title_in_byte_order_then_slug`.
//! - A REFUSED PATCH. The double applied a bulletin patch field by
//!   field and validated as it went, so a patch carrying a good title
//!   and an empty body was refused AFTER the title was written — the
//!   refusal left the row half-patched, where Postgres writes nothing.
//!   The double now validates the whole patch before touching the row.
//!   Case `a_refused_bulletin_patch_changes_nothing`.
//!
//! And one where the answers agreed only in kind: a section created on
//! a taken slug or under a missing parent was `Validation` on both, but
//! Postgres carried the raw constraint text (and called ANY insert
//! failure, a lost connection included, a validation failure). It now
//! names the slug the double's words name, and an error that is no
//! constraint is `Storage`. Case
//! `a_section_create_is_refused_on_a_blank_slug_or_title_a_taken_slug_or_a_missing_parent`.
//!
//! THE SHAPE, the other suites': each case states its answer and each
//! adapter is held to that stated answer, not merely to the other one.
//! Bulletin instants are whole seconds and every bulletin write passes
//! its own `now`, so Postgres's microseconds and the double's
//! nanoseconds compare equal; the manual's writes take no `now`, so its
//! cases compare a section's instants only with each other. The
//! migrations seed no bulletin and no section, and both adapters start
//! empty. The outbox events `PgContent` records are not the port's to
//! answer — the double records none — and are not judged here.

use boss_content::types::{
    Audience, Bulletin, BulletinDraft, BulletinPatch, BulletinPriority, ManualPatch, ManualSection,
    ManualSectionDraft, UserContext,
};
use boss_content::{ContentError, ContentRepository, InMemoryContent, PgContent};
use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde_json::json;
use uuid::Uuid;

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemoryContent::new(), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (PgContent::new(db.pool.clone()), db)
        },
    }
    cases {
        an_empty_store_answers_nothing,
        a_bulletin_reads_back_as_written,
        a_retried_create_with_the_same_id_answers_the_first_row,
        a_bulletin_without_a_title_or_body_is_refused,
        the_board_is_the_viewers_live_rows_by_priority_posted_created_then_id,
        a_dismissal_hides_a_bulletin_from_its_viewer_only,
        the_admin_list_is_every_bulletin_by_posted_then_created_then_id,
        a_bulletin_patch_replaces_only_the_fields_it_carries,
        a_refused_bulletin_patch_changes_nothing,
        a_delete_removes_the_bulletin_and_its_dismissals,
        a_section_reads_back_as_created_with_its_first_version,
        a_section_create_is_refused_on_a_blank_slug_or_title_a_taken_slug_or_a_missing_parent,
        a_section_update_bumps_the_version_and_appends_history_newest_first,
        a_refused_section_update_changes_nothing,
        a_reader_sees_only_published_sections_in_their_audience,
        the_tree_is_by_parent_then_sort_order_then_title_in_byte_order_then_slug,
    }
}

// ----- fixtures ------------------------------------------------------------

fn at(d: u32, h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, d, h, 0, 0).unwrap()
}

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
}

fn stamp() -> EventStamp {
    EventStamp::new("content", ActorId::Human("emp-suite".into()))
}

fn user(id: &str, role: &str, department: Option<&str>) -> UserContext {
    UserContext {
        id: id.into(),
        role: role.into(),
        department: department.map(str::to_string),
    }
}

fn viewer() -> UserContext {
    user("emp-viewer", "brewer", Some("brewing"))
}

fn draft(id: u128, title: &str) -> BulletinDraft {
    BulletinDraft {
        id: Some(Uuid::from_u128(id)),
        title: title.into(),
        body: format!("{title} body"),
        posted_on: Some(day(10)),
        expires_on: None,
        priority: BulletinPriority::Normal,
        audience: Audience::all(),
    }
}

async fn post<C: ContentRepository>(c: &C, d: BulletinDraft, now: DateTime<Utc>) -> Bulletin {
    c.create_bulletin_at(d, "emp-author", now, &stamp())
        .await
        .expect("create answers")
}

fn titles(rows: &[Bulletin]) -> Vec<&str> {
    rows.iter().map(|b| b.title.as_str()).collect()
}

fn section(slug: &str, parent: Option<&str>, title: &str) -> ManualSectionDraft {
    ManualSectionDraft {
        slug: slug.into(),
        parent_slug: parent.map(str::to_string),
        title: title.into(),
        body: format!("{title} body"),
        sort_order: 0,
        audience: Audience::all(),
        published: true,
    }
}

async fn write<C: ContentRepository>(c: &C, d: ManualSectionDraft) -> ManualSection {
    c.create_section(d, "emp-editor")
        .await
        .expect("create_section answers")
}

fn slugs(rows: &[ManualSection]) -> Vec<&str> {
    rows.iter().map(|s| s.slug.as_str()).collect()
}

fn validation(
    adapter: &str,
    what: &str,
    got: Result<impl std::fmt::Debug, ContentError>,
) -> String {
    match got {
        Err(ContentError::Validation(msg)) => msg,
        other => panic!("{adapter}: {what} expected Validation, got {other:?}"),
    }
}

fn not_found(adapter: &str, what: &str, got: Result<impl std::fmt::Debug, ContentError>) -> String {
    match got {
        Err(ContentError::NotFound(msg)) => msg,
        other => panic!("{adapter}: {what} expected NotFound, got {other:?}"),
    }
}

// ----- cases: bulletins ----------------------------------------------------

/// Nothing written: every read answers empty or absent — and a section's
/// history, which names a section, answers NotFound naming it.
async fn an_empty_store_answers_nothing<C: ContentRepository>(c: &C, adapter: &str) {
    let board = c
        .list_bulletins_for(&viewer(), day(10), true)
        .await
        .expect("board");
    assert!(board.is_empty(), "{adapter}: {board:?}");
    let all = c.list_all_bulletins().await.expect("list all");
    assert!(all.is_empty(), "{adapter}: {all:?}");
    assert_eq!(
        c.get_bulletin(Uuid::from_u128(1)).await.expect("get"),
        None,
        "{adapter}"
    );
    let tree = c.manual_tree(&viewer()).await.expect("tree");
    assert!(tree.is_empty(), "{adapter}: {tree:?}");
    assert_eq!(
        c.get_section("benefits", &viewer()).await.expect("get"),
        None,
        "{adapter}"
    );
    assert_eq!(
        not_found(adapter, "history", c.section_history("benefits").await),
        "section benefits",
        "{adapter}"
    );
}

/// A created bulletin answers every field it was drafted with, the
/// author, and the `now` it was written at as both instants; a draft
/// without `posted_on` is posted on the day of its `now`. `get`, the
/// admin list and the board answer the same row.
async fn a_bulletin_reads_back_as_written<C: ContentRepository>(c: &C, adapter: &str) {
    let d = BulletinDraft {
        id: Some(Uuid::from_u128(7)),
        title: "Tank 4 is down".into(),
        body: "Descale Thursday".into(),
        posted_on: None,
        expires_on: Some(day(20)),
        priority: BulletinPriority::Pinned,
        audience: Audience(json!({ "departments": ["brewing"] })),
    };
    let got = post(c, d, at(12, 9)).await;
    let want = Bulletin {
        id: Uuid::from_u128(7),
        title: "Tank 4 is down".into(),
        body: "Descale Thursday".into(),
        actor_id: "emp-author".into(),
        posted_on: day(12),
        expires_on: Some(day(20)),
        priority: BulletinPriority::Pinned,
        audience: Audience(json!({ "departments": ["brewing"] })),
        created_at: at(12, 9),
        updated_at: at(12, 9),
        dismissed_by_viewer: false,
    };
    assert_eq!(got, want, "{adapter}");
    assert_eq!(
        c.get_bulletin(want.id).await.expect("get"),
        Some(want.clone()),
        "{adapter}"
    );
    assert_eq!(
        c.list_all_bulletins().await.expect("list all"),
        vec![want.clone()],
        "{adapter}"
    );
    assert_eq!(
        c.list_bulletins_for(&viewer(), day(12), false)
            .await
            .expect("board"),
        vec![want],
        "{adapter}"
    );
}

/// A second create carrying an id already held is the retry of the
/// first: it answers the row the first wrote, and writes nothing.
async fn a_retried_create_with_the_same_id_answers_the_first_row<C: ContentRepository>(
    c: &C,
    adapter: &str,
) {
    let first = post(c, draft(1, "first"), at(10, 8)).await;
    let retry = post(c, draft(1, "retry"), at(10, 9)).await;
    assert_eq!(retry, first, "{adapter}");
    assert_eq!(
        c.list_all_bulletins().await.expect("list all"),
        vec![first],
        "{adapter}"
    );
}

/// A blank title or body is refused, naming the field, and writes
/// nothing.
async fn a_bulletin_without_a_title_or_body_is_refused<C: ContentRepository>(c: &C, adapter: &str) {
    let mut no_title = draft(1, "x");
    no_title.title = "  ".into();
    assert_eq!(
        validation(
            adapter,
            "blank title",
            c.create_bulletin_at(no_title, "emp-author", at(10, 8), &stamp())
                .await
        ),
        "title is required",
        "{adapter}"
    );
    let mut no_body = draft(2, "x");
    no_body.body = String::new();
    assert_eq!(
        validation(
            adapter,
            "blank body",
            c.create_bulletin_at(no_body, "emp-author", at(10, 8), &stamp())
                .await
        ),
        "body is required",
        "{adapter}"
    );
    let all = c.list_all_bulletins().await.expect("list all");
    assert!(all.is_empty(), "{adapter}: {all:?}");
}

/// The board is the rows live on `today` — one expiring today is still
/// live, one expired yesterday is not — whose audience holds the
/// viewer, urgent before pinned before normal, then newest posted,
/// then newest created, then by id where all three tie.
async fn the_board_is_the_viewers_live_rows_by_priority_posted_created_then_id<
    C: ContentRepository,
>(
    c: &C,
    adapter: &str,
) {
    let with = |id: u128,
                title: &str,
                priority: BulletinPriority,
                posted: u32,
                expires: Option<u32>,
                audience: serde_json::Value| {
        let mut d = draft(id, title);
        d.priority = priority;
        d.posted_on = Some(day(posted));
        d.expires_on = expires.map(day);
        d.audience = Audience(audience);
        d
    };
    let all = json!({ "all": true });
    // Written in an order no answer should follow; the tie pair is
    // written higher id first.
    post(
        c,
        with(9, "tie-9", BulletinPriority::Normal, 10, None, all.clone()),
        at(10, 8),
    )
    .await;
    post(
        c,
        with(3, "tie-3", BulletinPriority::Normal, 10, None, all.clone()),
        at(10, 8),
    )
    .await;
    post(
        c,
        with(
            5,
            "older-created",
            BulletinPriority::Normal,
            10,
            None,
            all.clone(),
        ),
        at(10, 7),
    )
    .await;
    post(
        c,
        with(
            6,
            "newer-posted",
            BulletinPriority::Normal,
            11,
            None,
            all.clone(),
        ),
        at(10, 6),
    )
    .await;
    post(
        c,
        with(7, "pinned", BulletinPriority::Pinned, 1, None, all.clone()),
        at(10, 5),
    )
    .await;
    post(
        c,
        with(
            8,
            "urgent",
            BulletinPriority::Urgent,
            1,
            Some(12),
            all.clone(),
        ),
        at(10, 4),
    )
    .await;
    post(
        c,
        with(
            10,
            "expired",
            BulletinPriority::Urgent,
            1,
            Some(11),
            all.clone(),
        ),
        at(10, 3),
    )
    .await;
    post(
        c,
        with(
            11,
            "other-dept",
            BulletinPriority::Urgent,
            1,
            None,
            json!({ "departments": ["sales"] }),
        ),
        at(10, 3),
    )
    .await;
    post(
        c,
        with(
            12,
            "my-role",
            BulletinPriority::Normal,
            1,
            None,
            json!({ "roles": ["brewer"] }),
        ),
        at(10, 3),
    )
    .await;

    let board = c
        .list_bulletins_for(&viewer(), day(12), false)
        .await
        .expect("board");
    assert_eq!(
        titles(&board),
        vec![
            "urgent",
            "pinned",
            "newer-posted",
            "tie-3",
            "tie-9",
            "older-created",
            "my-role"
        ],
        "{adapter}"
    );
}

/// A dismissal hides the bulletin from the board of the one who
/// dismissed it, unless they ask for dismissed rows, which answer it
/// flagged; another viewer still sees it unflagged, and `get` carries no
/// viewer. A second dismissal is a no-op; dismissing a bulletin the
/// store never held is NotFound, naming it.
async fn a_dismissal_hides_a_bulletin_from_its_viewer_only<C: ContentRepository>(
    c: &C,
    adapter: &str,
) {
    let b = post(c, draft(1, "notice"), at(10, 8)).await;
    post(c, draft(2, "other"), at(10, 7)).await;
    c.dismiss_bulletin_at(b.id, "emp-viewer", at(10, 9), &stamp())
        .await
        .expect("dismiss");
    c.dismiss_bulletin_at(b.id, "emp-viewer", at(10, 10), &stamp())
        .await
        .expect("dismiss again");

    let hidden = c
        .list_bulletins_for(&viewer(), day(10), false)
        .await
        .expect("board");
    assert_eq!(titles(&hidden), vec!["other"], "{adapter}");
    let shown = c
        .list_bulletins_for(&viewer(), day(10), true)
        .await
        .expect("board with dismissed");
    let flags: Vec<(&str, bool)> = shown
        .iter()
        .map(|b| (b.title.as_str(), b.dismissed_by_viewer))
        .collect();
    assert_eq!(flags, vec![("notice", true), ("other", false)], "{adapter}");
    let someone_else = c
        .list_bulletins_for(&user("emp-other", "brewer", None), day(10), false)
        .await
        .expect("board");
    assert_eq!(titles(&someone_else), vec!["notice", "other"], "{adapter}");
    assert!(
        someone_else.iter().all(|b| !b.dismissed_by_viewer),
        "{adapter}"
    );
    assert_eq!(
        c.get_bulletin(b.id).await.expect("get"),
        Some(b),
        "{adapter}"
    );
    let unknown = Uuid::from_u128(99);
    assert_eq!(
        not_found(
            adapter,
            "dismiss unknown",
            c.dismiss_bulletin_at(unknown, "emp-viewer", at(10, 9), &stamp())
                .await
        ),
        format!("bulletin {unknown}"),
        "{adapter}"
    );
}

/// The admin list is every bulletin, expired and audience-scoped ones
/// included, newest posted first, then newest created, then by id where
/// both tie — and carries no viewer's dismissal.
async fn the_admin_list_is_every_bulletin_by_posted_then_created_then_id<C: ContentRepository>(
    c: &C,
    adapter: &str,
) {
    let with = |id: u128, title: &str, posted: u32, expires: Option<u32>| {
        let mut d = draft(id, title);
        d.posted_on = Some(day(posted));
        d.expires_on = expires.map(day);
        d
    };
    post(c, with(9, "tie-9", 10, None), at(10, 8)).await;
    post(c, with(4, "older-created", 10, None), at(10, 7)).await;
    post(c, with(3, "tie-3", 10, None), at(10, 8)).await;
    post(c, with(5, "newer-created", 10, None), at(10, 9)).await;
    post(c, with(6, "expired", 11, Some(11)), at(10, 1)).await;
    let mut scoped = with(7, "scoped", 2, None);
    scoped.audience = Audience(json!({ "roles": ["nobody"] }));
    post(c, scoped, at(10, 1)).await;
    c.dismiss_bulletin_at(Uuid::from_u128(3), "emp-viewer", at(10, 9), &stamp())
        .await
        .expect("dismiss");

    let all = c.list_all_bulletins().await.expect("list all");
    assert_eq!(
        titles(&all),
        vec![
            "expired",
            "newer-created",
            "tie-3",
            "tie-9",
            "older-created",
            "scoped"
        ],
        "{adapter}"
    );
    assert!(all.iter().all(|b| !b.dismissed_by_viewer), "{adapter}");
}

/// A patch replaces the fields it carries and keeps the rest — an
/// `expires_on` of `Some(None)` clears the expiry — and stamps
/// `updated_at` with its `now`, keeping author, posting and creation.
async fn a_bulletin_patch_replaces_only_the_fields_it_carries<C: ContentRepository>(
    c: &C,
    adapter: &str,
) {
    let mut d = draft(1, "notice");
    d.expires_on = Some(day(20));
    let before = post(c, d, at(10, 8)).await;

    let patched = c
        .update_bulletin_at(
            before.id,
            BulletinPatch {
                title: Some("renamed".into()),
                body: None,
                expires_on: Some(None),
                priority: Some(BulletinPriority::Urgent),
                audience: Some(Audience(json!({ "roles": ["brewer"] }))),
            },
            at(11, 9),
            &stamp(),
        )
        .await
        .expect("patch");
    let want = Bulletin {
        title: "renamed".into(),
        expires_on: None,
        priority: BulletinPriority::Urgent,
        audience: Audience(json!({ "roles": ["brewer"] })),
        updated_at: at(11, 9),
        ..before
    };
    assert_eq!(patched, want, "{adapter}");
    assert_eq!(
        c.get_bulletin(want.id).await.expect("get"),
        Some(want.clone()),
        "{adapter}"
    );

    let body_only = c
        .update_bulletin_at(
            want.id,
            BulletinPatch {
                body: Some("new body".into()),
                ..BulletinPatch::default()
            },
            at(12, 9),
            &stamp(),
        )
        .await
        .expect("patch body");
    assert_eq!(
        body_only,
        Bulletin {
            body: "new body".into(),
            updated_at: at(12, 9),
            ..want
        },
        "{adapter}"
    );
}

/// A patch carrying a blank title or a blank body is refused, naming
/// the field, and changes NOTHING — not even a good field the same
/// patch carried. A patch of a bulletin the store never held is
/// NotFound, naming it.
async fn a_refused_bulletin_patch_changes_nothing<C: ContentRepository>(c: &C, adapter: &str) {
    let before = post(c, draft(1, "notice"), at(10, 8)).await;
    let refusals = [
        (
            BulletinPatch {
                title: Some(" ".into()),
                body: Some("good body".into()),
                ..BulletinPatch::default()
            },
            "title must not be empty",
        ),
        (
            BulletinPatch {
                title: Some("good title".into()),
                body: Some(String::new()),
                priority: Some(BulletinPriority::Urgent),
                ..BulletinPatch::default()
            },
            "body must not be empty",
        ),
    ];
    for (patch, words) in refusals {
        assert_eq!(
            validation(
                adapter,
                words,
                c.update_bulletin_at(before.id, patch, at(11, 9), &stamp())
                    .await
            ),
            words,
            "{adapter}"
        );
        assert_eq!(
            c.get_bulletin(before.id).await.expect("get"),
            Some(before.clone()),
            "{adapter}: a refused patch ({words}) left a trace"
        );
    }
    let unknown = Uuid::from_u128(99);
    assert_eq!(
        not_found(
            adapter,
            "patch unknown",
            c.update_bulletin_at(unknown, BulletinPatch::default(), at(11, 9), &stamp())
                .await
        ),
        format!("bulletin {unknown}"),
        "{adapter}"
    );
}

/// A delete removes the bulletin and every dismissal of it: a second
/// delete is NotFound, and a bulletin later created on the same id is
/// not dismissed for the viewer who dismissed the first.
async fn a_delete_removes_the_bulletin_and_its_dismissals<C: ContentRepository>(
    c: &C,
    adapter: &str,
) {
    let b = post(c, draft(1, "notice"), at(10, 8)).await;
    c.dismiss_bulletin_at(b.id, "emp-viewer", at(10, 9), &stamp())
        .await
        .expect("dismiss");
    c.delete_bulletin_at(b.id, at(10, 10), &stamp())
        .await
        .expect("delete");
    assert_eq!(c.get_bulletin(b.id).await.expect("get"), None, "{adapter}");
    assert_eq!(
        not_found(
            adapter,
            "delete again",
            c.delete_bulletin_at(b.id, at(10, 11), &stamp()).await
        ),
        format!("bulletin {}", b.id),
        "{adapter}"
    );
    post(c, draft(1, "reposted"), at(10, 12)).await;
    let board = c
        .list_bulletins_for(&viewer(), day(10), false)
        .await
        .expect("board");
    assert_eq!(titles(&board), vec!["reposted"], "{adapter}");
}

// ----- cases: the manual ---------------------------------------------------

/// A created section answers every field it was drafted with at version
/// 1, created and updated at one instant; the reader reads the same
/// row; its history is one version, the draft's text, by its editor, at
/// that instant, reason "initial version".
async fn a_section_reads_back_as_created_with_its_first_version<C: ContentRepository>(
    c: &C,
    adapter: &str,
) {
    write(c, section("benefits", None, "Benefits")).await;
    let d = ManualSectionDraft {
        slug: "benefits/pto".into(),
        parent_slug: Some("benefits".into()),
        title: "Time off".into(),
        body: "Twenty days".into(),
        sort_order: 3,
        audience: Audience(json!({ "departments": ["brewing"] })),
        published: true,
    };
    let s = write(c, d).await;
    assert_eq!(
        (
            s.slug.as_str(),
            s.parent_slug.as_deref(),
            s.title.as_str(),
            s.body.as_str(),
            s.sort_order,
            &s.audience,
            s.current_version,
            s.published,
        ),
        (
            "benefits/pto",
            Some("benefits"),
            "Time off",
            "Twenty days",
            3,
            &Audience(json!({ "departments": ["brewing"] })),
            1,
            true,
        ),
        "{adapter}"
    );
    assert_eq!(s.created_at, s.updated_at, "{adapter}");
    assert_eq!(
        c.get_section("benefits/pto", &viewer()).await.expect("get"),
        Some(s.clone()),
        "{adapter}"
    );
    let history = c.section_history("benefits/pto").await.expect("history");
    assert_eq!(history.len(), 1, "{adapter}: {history:?}");
    let v1 = &history[0];
    assert_eq!(
        (
            v1.section_id,
            v1.version,
            v1.title.as_str(),
            v1.body.as_str(),
            &v1.audience,
            v1.edited_by.as_str(),
            v1.edited_at,
            v1.reason.as_deref(),
        ),
        (
            s.id,
            1,
            "Time off",
            "Twenty days",
            &s.audience,
            "emp-editor",
            s.created_at,
            Some("initial version"),
        ),
        "{adapter}"
    );
}

/// A blank slug or title, a slug already held, and a parent the manual
/// does not hold are each refused as a validation failure in the same
/// words, and leave the manual as it was.
async fn a_section_create_is_refused_on_a_blank_slug_or_title_a_taken_slug_or_a_missing_parent<
    C: ContentRepository,
>(
    c: &C,
    adapter: &str,
) {
    let held = write(c, section("benefits", None, "Benefits")).await;
    let refusals = [
        (
            section(" ", None, "Blank slug"),
            "slug is required".to_string(),
        ),
        (section("x", None, ""), "title is required".to_string()),
        (
            section("benefits", None, "Again"),
            "slug 'benefits' already exists".to_string(),
        ),
        (
            section("orphan", Some("no-such-parent"), "Orphan"),
            "parent slug 'no-such-parent' not found".to_string(),
        ),
    ];
    for (d, words) in refusals {
        assert_eq!(
            validation(adapter, &words, c.create_section(d, "emp-editor").await),
            words,
            "{adapter}"
        );
    }
    let tree = c.manual_tree(&viewer()).await.expect("tree");
    assert_eq!(tree, vec![held], "{adapter}");
    assert_eq!(
        not_found(adapter, "orphan history", c.section_history("orphan").await),
        "section orphan",
        "{adapter}"
    );
}

/// Each update applies the fields its patch carries, keeps the rest,
/// bumps `current_version` by one and appends a history row holding
/// the NEW text, its editor and reason, stamped at the section's new
/// `updated_at`. History answers newest first.
async fn a_section_update_bumps_the_version_and_appends_history_newest_first<
    C: ContentRepository,
>(
    c: &C,
    adapter: &str,
) {
    let created = write(c, section("benefits", None, "Benefits")).await;
    let v2 = c
        .update_section(
            "benefits",
            ManualPatch {
                title: Some("Benefits & perks".into()),
                sort_order: Some(5),
                reason: Some("rename".into()),
                ..ManualPatch::default()
            },
            "emp-hr",
        )
        .await
        .expect("update");
    assert_eq!(
        (
            v2.id,
            v2.title.as_str(),
            v2.body.as_str(),
            v2.sort_order,
            v2.current_version,
            v2.created_at,
        ),
        (
            created.id,
            "Benefits & perks",
            "Benefits body",
            5,
            2,
            created.created_at
        ),
        "{adapter}"
    );
    let v3 = c
        .update_section(
            "benefits",
            ManualPatch {
                body: Some("Rewritten".into()),
                audience: Some(Audience(json!({ "roles": ["brewer"] }))),
                ..ManualPatch::default()
            },
            "emp-hr2",
        )
        .await
        .expect("update again");
    assert_eq!(
        (v3.title.as_str(), v3.body.as_str(), v3.current_version),
        ("Benefits & perks", "Rewritten", 3),
        "{adapter}"
    );
    assert_eq!(
        c.get_section("benefits", &viewer()).await.expect("get"),
        Some(v3.clone()),
        "{adapter}"
    );

    let history = c.section_history("benefits").await.expect("history");
    let rows: Vec<(i32, &str, &str, &str, Option<&str>)> = history
        .iter()
        .map(|v| {
            (
                v.version,
                v.title.as_str(),
                v.body.as_str(),
                v.edited_by.as_str(),
                v.reason.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (3, "Benefits & perks", "Rewritten", "emp-hr2", None),
            (
                2,
                "Benefits & perks",
                "Benefits body",
                "emp-hr",
                Some("rename")
            ),
            (
                1,
                "Benefits",
                "Benefits body",
                "emp-editor",
                Some("initial version")
            ),
        ],
        "{adapter}"
    );
    assert_eq!(history[0].edited_at, v3.updated_at, "{adapter}");
    assert_eq!(history[0].audience, v3.audience, "{adapter}");
    assert!(
        history.iter().all(|v| v.section_id == created.id),
        "{adapter}"
    );
}

/// An update carrying a blank title is refused and changes nothing —
/// neither the section nor its history. Updating or asking the history
/// of a slug the manual does not hold is NotFound, naming it.
async fn a_refused_section_update_changes_nothing<C: ContentRepository>(c: &C, adapter: &str) {
    let created = write(c, section("benefits", None, "Benefits")).await;
    assert_eq!(
        validation(
            adapter,
            "blank title",
            c.update_section(
                "benefits",
                ManualPatch {
                    title: Some("  ".into()),
                    body: Some("good body".into()),
                    ..ManualPatch::default()
                },
                "emp-hr",
            )
            .await
        ),
        "title must not be empty",
        "{adapter}"
    );
    assert_eq!(
        c.get_section("benefits", &viewer()).await.expect("get"),
        Some(created),
        "{adapter}"
    );
    assert_eq!(
        c.section_history("benefits").await.expect("history").len(),
        1,
        "{adapter}"
    );
    assert_eq!(
        not_found(
            adapter,
            "update unknown",
            c.update_section("nope", ManualPatch::default(), "emp-hr")
                .await
        ),
        "section nope",
        "{adapter}"
    );
}

/// The reader — `get_section` and the tree — sees a section only while
/// it is published and its audience holds the reader. Unpublishing is
/// an update like any other; republishing brings it back.
async fn a_reader_sees_only_published_sections_in_their_audience<C: ContentRepository>(
    c: &C,
    adapter: &str,
) {
    write(c, section("open", None, "Open")).await;
    let mut draft_only = section("draft", None, "Draft");
    draft_only.published = false;
    write(c, draft_only).await;
    let mut sales = section("sales", None, "Sales");
    sales.audience = Audience(json!({ "departments": ["sales"] }));
    write(c, sales).await;
    let mut brewers = section("brewers", None, "Brewers");
    brewers.audience = Audience(json!({ "roles": ["brewer"] }));
    write(c, brewers).await;

    let tree = c.manual_tree(&viewer()).await.expect("tree");
    assert_eq!(slugs(&tree), vec!["brewers", "open"], "{adapter}");
    for hidden in ["draft", "sales"] {
        assert_eq!(
            c.get_section(hidden, &viewer()).await.expect("get"),
            None,
            "{adapter}: {hidden}"
        );
    }
    let sales_rep = user("emp-s", "rep", Some("sales"));
    let tree = c.manual_tree(&sales_rep).await.expect("tree");
    assert_eq!(slugs(&tree), vec!["open", "sales"], "{adapter}");

    let unpublish = ManualPatch {
        published: Some(false),
        ..ManualPatch::default()
    };
    c.update_section("open", unpublish, "emp-hr")
        .await
        .expect("unpublish");
    assert_eq!(
        c.get_section("open", &viewer()).await.expect("get"),
        None,
        "{adapter}"
    );
    let republish = ManualPatch {
        published: Some(true),
        ..ManualPatch::default()
    };
    c.update_section("open", republish, "emp-hr")
        .await
        .expect("republish");
    let tree = c.manual_tree(&viewer()).await.expect("tree");
    assert_eq!(slugs(&tree), vec!["brewers", "open"], "{adapter}");
}

/// The tree answers roots first, then by parent slug, then sort order,
/// then title — both texts in BYTE order (`-` before a letter, upper
/// before lower), the only order both adapters can hold — then by the
/// unique slug where title and sort order tie.
async fn the_tree_is_by_parent_then_sort_order_then_title_in_byte_order_then_slug<
    C: ContentRepository,
>(
    c: &C,
    adapter: &str,
) {
    let with = |slug: &str, parent: Option<&str>, title: &str, order: i32| {
        let mut d = section(slug, parent, title);
        d.sort_order = order;
        d
    };
    // Two roots tied on (no parent, 0, "root"), so the slug decides
    // them; and as parents they sort the other way round in a locale,
    // which ignores `-` and reads "suiteab" before "suiteaz", where
    // bytes put `-` (0x2d) before `b` (0x62).
    write(c, with("suite-ab", None, "root", 0)).await;
    write(c, with("suite-a-z", None, "root", 0)).await;
    // Under suite-ab: sort order beats title; titles in byte order
    // (`B` before `a`); the tie pair written higher slug first.
    write(c, with("ab/late", Some("suite-ab"), "aaa", 2)).await;
    write(c, with("ab/lower", Some("suite-ab"), "alpha", 1)).await;
    write(c, with("ab/upper", Some("suite-ab"), "Beta", 1)).await;
    write(c, with("ab/tie-z", Some("suite-ab"), "same", 1)).await;
    write(c, with("ab/tie-a", Some("suite-ab"), "same", 1)).await;
    write(c, with("az/only", Some("suite-a-z"), "only", 0)).await;

    let tree = c.manual_tree(&viewer()).await.expect("tree");
    assert_eq!(
        slugs(&tree),
        vec![
            "suite-a-z",
            "suite-ab",
            "az/only",
            "ab/upper",
            "ab/lower",
            "ab/tie-a",
            "ab/tie-z",
            "ab/late",
        ],
        "{adapter}"
    );
}
