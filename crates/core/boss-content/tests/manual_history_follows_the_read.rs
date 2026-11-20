//! THE HISTORY ROUTE ANSWERS ONLY WHAT THE READ WOULD (backlog 18345a5b).
//!
//! `GET /api/content/manual/{slug}` reads through `get_section(&slug,
//! &user)`, which hides an unpublished section and one whose audience
//! does not match the caller. `GET /api/content/manual-history/{slug}`
//! took no caller at all and returned every version's full body, so a
//! section the read refused was readable through its history. Found by
//! the /manual page-audit analyst (run aae8a58d, 2026-09-23). The route
//! now asks the same question first and answers the read's 404 when the
//! caller cannot see the section.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_content::http::{ContentApiState, router};
use boss_content::types::{Audience, ManualSectionDraft};
use boss_content::{ContentRepository, InMemoryContent};
use tower::ServiceExt;

fn draft(slug: &str, audience: Audience, published: bool) -> ManualSectionDraft {
    ManualSectionDraft {
        slug: slug.into(),
        parent_slug: None,
        title: slug.into(),
        body: format!("the restricted body of {slug}"),
        sort_order: 0,
        audience,
        published,
        owner_station: None,
        authoring_job_id: None,
    }
}

fn user(department: &str) -> String {
    serde_json::json!({ "id": "emp-reader", "role": "staff", "department": department }).to_string()
}

async fn get(app: &axum::Router, path: &str, who: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .uri(path)
        .header("x-boss-user", who)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn app() -> axum::Router {
    let repo = InMemoryContent::new();
    let sales_only = Audience(serde_json::json!({ "departments": ["sales"] }));
    repo.create_section(draft("sales-comp", sales_only, true), "hr-01")
        .await
        .unwrap();
    repo.create_section(draft("draft-policy", Audience::all(), false), "hr-01")
        .await
        .unwrap();
    repo.create_section(draft("welcome", Audience::all(), true), "hr-01")
        .await
        .unwrap();
    router(ContentApiState {
        repo: Arc::new(repo),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    })
}

#[tokio::test]
async fn a_section_the_read_refuses_has_no_readable_history() {
    let app = app().await;
    for slug in ["sales-comp", "draft-policy"] {
        let (read, _) = get(&app, &format!("/api/content/manual/{slug}"), &user("it")).await;
        assert_eq!(read, StatusCode::NOT_FOUND, "{slug}: the read refuses");
        let (hist, body) = get(
            &app,
            &format!("/api/content/manual-history/{slug}"),
            &user("it"),
        )
        .await;
        assert_eq!(
            hist,
            StatusCode::NOT_FOUND,
            "{slug}: the history answers what the read answers, not {body}"
        );
        assert!(!body.contains("restricted body"), "{slug}: {body}");
    }
}

#[tokio::test]
async fn a_section_the_caller_can_read_still_has_its_history() {
    let app = app().await;
    for (slug, dept) in [("sales-comp", "sales"), ("welcome", "it")] {
        let (hist, body) = get(
            &app,
            &format!("/api/content/manual-history/{slug}"),
            &user(dept),
        )
        .await;
        assert_eq!(hist, StatusCode::OK, "{slug}: {body}");
        assert!(body.contains("restricted body"), "{slug}: {body}");
    }
}

#[tokio::test]
async fn the_history_needs_a_caller() {
    let app = app().await;
    let req = Request::builder()
        .uri("/api/content/manual-history/welcome")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_manual_index_names_unwritten_work_without_disclosing_its_draft() {
    let repo = InMemoryContent::new();
    let packet = "2c28833a-d0a3-499a-bcf9-2d3a264e26a3";
    let unwritten: ManualSectionDraft = serde_json::from_value(serde_json::json!({
        "slug": "expenses", "title": "Expenses", "body": "PRIVATE unfinished draft",
        "published": false, "owner_station": "finance", "authoring_job_id": packet,
    }))
    .unwrap();
    repo.create_section(unwritten, "hr-01").await.unwrap();
    repo.create_section(draft("welcome", Audience::all(), true), "hr-01")
        .await
        .unwrap();
    repo.create_section(
        draft(
            "sales-only",
            Audience(serde_json::json!({"departments": ["sales"]})),
            false,
        ),
        "hr-01",
    )
    .await
    .unwrap();
    let app = router(ContentApiState {
        repo: Arc::new(repo),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    });
    let (status, body) = get(&app, "/api/content/manual-index", &user("it")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the index must expose safe headers: {body}"
    );
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(
        rows.len(),
        2,
        "the audience still controls section visibility"
    );
    let row = rows.iter().find(|row| row["slug"] == "expenses").unwrap();
    assert_eq!(row["published"], false);
    assert_eq!(row["owner_station"], "finance");
    assert_eq!(row["authoring_job_id"], packet);
    assert!(rows.iter().all(|row| row.get("body").is_none()));
    assert!(!body.contains("PRIVATE"));
    assert_eq!(
        get(&app, "/api/content/manual/expenses", &user("it"))
            .await
            .0,
        StatusCode::NOT_FOUND,
        "the body remains unpublished",
    );
    assert_eq!(
        get(&app, "/api/content/manual-history/expenses", &user("it"))
            .await
            .0,
        StatusCode::NOT_FOUND,
        "the index must not open draft history",
    );
    assert_eq!(
        get(&app, "/api/content/manual/welcome", &user("it"))
            .await
            .0,
        StatusCode::OK,
        "published content remains readable",
    );
}

#[tokio::test]
async fn an_unwritten_starter_survives_the_next_seed_without_being_published() {
    let repo = InMemoryContent::new();
    let original = repo
        .create_section(draft("welcome", Audience::all(), false), "emp-writer")
        .await
        .unwrap();
    let result = boss_content::seed::seed_starter_sections(&repo).await;
    assert!(
        result.is_ok(),
        "a hidden draft still owns its slug: {result:?}"
    );
    let user = boss_content::types::UserContext {
        id: "emp-reader".into(),
        role: "staff".into(),
        department: Some("it".into()),
    };
    let index = repo.manual_index(&user).await.unwrap();
    let welcome = index.iter().find(|s| s.slug == "welcome").unwrap();
    assert_eq!(welcome.id, original.id);
    assert!(!welcome.published);
    assert!(repo.get_section("welcome", &user).await.unwrap().is_none());
    assert_eq!(repo.section_history("welcome").await.unwrap().len(), 1);
}
