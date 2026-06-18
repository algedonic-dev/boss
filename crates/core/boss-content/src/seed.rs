//! Starter manual sections. Run once on startup — if a slug already
//! exists, it's left alone. The idea per the design doc is that the
//! manual has a meaningful skeleton on day one so HR edits placeholders
//! instead of staring at a blank tree.
//!
//! The data lives in `boss-content/seeds/manual_starter.toml`, bundled
//! into the binary via `include_str!` and parsed once into the static
//! `STARTERS` table. Edit the TOML to change titles, body templates,
//! or sort order; no Rust changes needed.

use std::sync::OnceLock;

use serde::Deserialize;

use crate::error::ContentError;
use crate::port::ContentRepository;
use crate::types::{Audience, ManualSectionDraft};

const STARTERS_TOML: &str = include_str!("../seeds/manual_starter.toml");

#[derive(Debug, Deserialize)]
struct StartersBundle {
    section: Vec<SeedSection>,
}

#[derive(Debug, Deserialize)]
struct SeedSection {
    slug: String,
    #[serde(default)]
    parent: Option<String>,
    title: String,
    body: String,
    sort: i32,
}

fn starters() -> &'static [SeedSection] {
    static CACHE: OnceLock<Vec<SeedSection>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let bundle: StartersBundle = toml::from_str(STARTERS_TOML)
            .expect("manual_starter.toml ships with the crate and must parse");
        bundle.section
    })
}

/// Create any starter section whose slug isn't already in the DB.
/// Returns the number of sections inserted.
pub async fn seed_starter_sections(repo: &dyn ContentRepository) -> Result<usize, ContentError> {
    // Read the full tree once with a system user — starters use the
    // default open audience, and we need to know which slugs exist.
    let user = crate::types::UserContext {
        id: "automation:content-seed".into(),
        role: "hr-lead".into(),
        department: Some("hr".into()),
    };
    let existing = repo.manual_tree(&user).await?;
    let have: std::collections::HashSet<String> = existing.into_iter().map(|s| s.slug).collect();

    let mut inserted = 0;
    for s in starters() {
        if have.contains(&s.slug) {
            continue;
        }
        let draft = ManualSectionDraft {
            slug: s.slug.clone(),
            parent_slug: s.parent.clone(),
            title: s.title.clone(),
            body: s.body.clone(),
            sort_order: s.sort,
            audience: Audience::all(),
            published: true,
        };
        repo.create_section(draft, "automation:content-seed")
            .await?;
        inserted += 1;
    }
    Ok(inserted)
}
